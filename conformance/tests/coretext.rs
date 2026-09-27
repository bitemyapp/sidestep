//! CoreText, checked against macOS: fonts made from a font file (DejaVu
//! Sans, in `fixtures/`, so the numbers are the same everywhere) and their
//! metrics, glyphs, advances, bounds and outlines; font descriptors and
//! feature settings; lines, runs, carets, truncation and justification;
//! typesetters and frames; paragraph styles; drawing glyphs and lines into
//! bitmap contexts; the constants; and the bridges (a CTFont is an
//! NSFont, a CFAttributedString an NSAttributedString).
//!
//! Glyph ids and font metrics are exact. Positions and widths are sums of
//! advances, which Sidestep's layout keeps in single precision: within
//! 0.001 point. Pixels allow for rasterizers antialiasing differently: ink
//! found within a pixel of where macOS puts it. CoreText runs on any
//! thread, so the default harness does.

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{AnyThread, msg_send};
use objc2_core_foundation::{
    CFAttributedString, CFCopyTypeIDDescription, CFData, CFDictionary, CFGetTypeID, CFIndex, CFMutableAttributedString,
    CFRange, CFRetained, CFString, CFType, CGAffineTransform, CGPoint, CGRect, CGSize, ConcreteType,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGBitmapContextGetHeight,
    CGBitmapContextGetWidth, CGColor, CGColorSpace, CGContext, CGDataProvider, CGFont, CGImageAlphaInfo, CGPath,
    CGTextDrawingMode, kCGColorSpaceSRGB,
};
use objc2_core_text::*;
use objc2_foundation::{
    NSArray, NSAttributedString, NSDictionary, NSMutableAttributedString, NSNumber, NSRange, NSString,
};

use sidestep as _;

const DEJAVU: &[u8] = include_bytes!("fixtures/DejaVuSans.ttf");

fn any<T: ?Sized>(x: &T) -> &AnyObject {
    // SAFETY: CoreFoundation's objects are objects.
    unsafe { &*(x as *const T as *const AnyObject) }
}

fn ns(s: &CFString) -> &NSString {
    // SAFETY: a CFString is an NSString.
    unsafe { &*(s as *const CFString as *const NSString) }
}

fn graphics_font() -> CFRetained<CGFont> {
    let data = CFData::from_bytes(DEJAVU);
    let provider = CGDataProvider::with_cf_data(Some(&data)).expect("a provider");
    CGFont::with_data_provider(&provider).expect("a font")
}

fn dejavu(size: f64) -> CFRetained<CTFont> {
    // SAFETY: no matrix, no attributes.
    unsafe { CTFont::with_graphics_font(&graphics_font(), size, std::ptr::null(), None) }
}

fn attributed(text: &str, pairs: &[(&CFString, &AnyObject)]) -> Retained<NSAttributedString> {
    let keys: Vec<&NSString> = pairs.iter().map(|p| ns(p.0)).collect();
    let values: Vec<&AnyObject> = pairs.iter().map(|p| p.1).collect();
    let dict = NSDictionary::from_slices(&keys, &values);
    // SAFETY: a string and an attribute dictionary.
    unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(text),
            Some(&dict),
        )
    }
}

fn cf(s: &NSAttributedString) -> &CFAttributedString {
    // SAFETY: an NSAttributedString is a CFAttributedString.
    unsafe { &*(s as *const NSAttributedString as *const CFAttributedString) }
}

fn line(text: &str, font: &CTFont, extra: &[(&CFString, &AnyObject)]) -> CFRetained<CTLine> {
    // SAFETY: the constant is a string.
    let mut pairs: Vec<(&CFString, &AnyObject)> = vec![(unsafe { kCTFontAttributeName }, any(font))];
    pairs.extend_from_slice(extra);
    // SAFETY: an attributed string.
    unsafe { CTLine::with_attributed_string(cf(&attributed(text, &pairs))) }
}

fn glyphs_of(font: &CTFont, text: &str) -> (Vec<u16>, bool) {
    let chars: Vec<u16> = text.encode_utf16().collect();
    let mut glyphs = vec![0u16; chars.len()];
    // SAFETY: as many glyphs as characters.
    let ok = unsafe {
        font.glyphs_for_characters(
            NonNull::new(chars.as_ptr().cast_mut()).unwrap(),
            NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            chars.len() as CFIndex,
        )
    };
    (glyphs, ok)
}

fn r(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

#[track_caller]
fn close(a: f64, b: f64, tolerance: f64) {
    assert!((a - b).abs() <= tolerance, "{a} is not {b}");
}

#[track_caller]
fn close_rect(a: CGRect, b: CGRect, tolerance: f64) {
    for (x, y) in [
        (a.origin.x, b.origin.x),
        (a.origin.y, b.origin.y),
        (a.size.width, b.size.width),
        (a.size.height, b.size.height),
    ] {
        assert!((x - y).abs() <= tolerance, "{a:?} is not {b:?}");
    }
}

/// A run of a line: glyphs, positions, advances, indices, string range,
/// status.
struct Run {
    glyphs: Vec<u16>,
    positions: Vec<(f64, f64)>,
    advances: Vec<f64>,
    indices: Vec<CFIndex>,
    range: CFRange,
    status: CTRunStatus,
    attributes: CFRetained<CFDictionary>,
}

fn runs(line: &CTLine) -> Vec<Run> {
    // SAFETY: a line's runs are CTRuns.
    let runs = unsafe { line.glyph_runs() };
    (0..runs.count())
        .map(|i| {
            // SAFETY: as above.
            let run: &CTRun = unsafe { &*(runs.value_at_index(i) as *const CTRun) };
            // SAFETY: buffers as long as the run.
            unsafe {
                let n = run.glyph_count() as usize;
                let mut glyphs = vec![0u16; n];
                let mut positions = vec![CGPoint::ZERO; n];
                let mut advances = vec![CGSize::ZERO; n];
                let mut indices = vec![0 as CFIndex; n];
                if n > 0 {
                    let all = CFRange::new(0, 0);
                    run.glyphs(all, NonNull::new(glyphs.as_mut_ptr()).unwrap());
                    run.positions(all, NonNull::new(positions.as_mut_ptr()).unwrap());
                    run.advances(all, NonNull::new(advances.as_mut_ptr()).unwrap());
                    run.string_indices(all, NonNull::new(indices.as_mut_ptr()).unwrap());
                }
                Run {
                    glyphs,
                    positions: positions.iter().map(|p| (p.x, p.y)).collect(),
                    advances: advances.iter().map(|a| a.width).collect(),
                    indices,
                    range: run.string_range(),
                    status: run.status(),
                    attributes: run.attributes(),
                }
            }
        })
        .collect()
}

#[track_caller]
fn close_all(a: &[f64], b: &[f64]) {
    assert_eq!(a.len(), b.len(), "{a:?} is not {b:?}");
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() <= 1e-3, "{a:?} is not {b:?}");
    }
}

fn typographic(line: &CTLine) -> (f64, f64, f64, f64) {
    let (mut a, mut d, mut l) = (0.0, 0.0, 0.0);
    // SAFETY: three floats to write.
    let w = unsafe { line.typographic_bounds(&mut a, &mut d, &mut l) };
    (w, a, d, l)
}

/// A `w` × `h` context: 8-bit RGBA, premultiplied, alpha last, sRGB.
fn ctx(w: usize, h: usize) -> CFRetained<CGContext> {
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).expect("sRGB");
    // SAFETY: no data: the context allocates.
    unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, Some(&space), CGImageAlphaInfo::PremultipliedLast.0)
    }
    .expect("a bitmap context")
}

fn px(c: &CGContext, x: usize, y: usize) -> [u8; 4] {
    let d = CGBitmapContextGetData(Some(c)) as *const u8;
    let bpr = CGBitmapContextGetBytesPerRow(Some(c));
    // SAFETY: inside the context's memory.
    unsafe { std::ptr::read(d.add(y * bpr + x * 4).cast::<[u8; 4]>()) }
}

/// The box of pixels with alpha above `t`: x0, y0 (rows from the top), x1,
/// y1.
fn ink(c: &CGContext, t: u8) -> Option<[usize; 4]> {
    let (w, h) = (CGBitmapContextGetWidth(Some(c)), CGBitmapContextGetHeight(Some(c)));
    let mut b: Option<[usize; 4]> = None;
    for y in 0..h {
        for x in 0..w {
            if px(c, x, y)[3] > t {
                b = Some(match b {
                    None => [x, y, x + 1, y + 1],
                    Some([a, bb, cc, d]) => [a.min(x), bb.min(y), cc.max(x + 1), d.max(y + 1)],
                });
            }
        }
    }
    b
}

/// Ink within a pixel of `want` (antialiasing differs at the edges).
#[track_caller]
fn ink_near(c: &CGContext, want: [usize; 4]) {
    let got = ink(c, 16).expect("ink");
    assert!(got.iter().zip(&want).all(|(a, b)| a.abs_diff(*b) <= 1), "ink {got:?}, want {want:?}");
}

/// Held while making AppKit's fonts: AppKit may not like several threads
/// making them at once (as `text_blocks.rs` and `text_layout.rs` found).
fn appkit_fonts() -> std::sync::MutexGuard<'static, ()> {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ONE.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn type_ids_and_bridges() {
    let names = [
        (CTFont::type_id(), "CTFont"),
        (CTFontDescriptor::type_id(), "CTFontDescriptor"),
        (CTLine::type_id(), "CTLine"),
        (CTRun::type_id(), "CTRun"),
        (CTTypesetter::type_id(), "CTTypesetter"),
        (CTFramesetter::type_id(), "CTFramesetter"),
        (CTFrame::type_id(), "CTFrame"),
        (CTParagraphStyle::type_id(), "CTParagraphStyle"),
        (CTFontCollection::type_id(), "CTFontCollection"),
        (CTGlyphInfo::type_id(), "CTGlyphInfo"),
        (CTRunDelegate::type_id(), "CTRunDelegate"),
        (CTTextTab::type_id(), "CTTextTab"),
        (CTRubyAnnotation::type_id(), "CTRubyAnnotation"),
    ];
    for (id, name) in names {
        assert_eq!(CFCopyTypeIDDescription(id).unwrap().to_string(), name);
    }
    for (i, a) in names.iter().enumerate() {
        for b in &names[i + 1..] {
            assert_ne!(a.0, b.0, "{} and {}", a.1, b.1);
        }
    }
    // A font made from a font file is an NSFont, and an NSFont a CTFont.
    let font = dejavu(24.0);
    assert_eq!(CFGetTypeID(Some(&font)), CTFont::type_id());
    let class = AnyClass::get(c"NSFont").unwrap();
    // SAFETY: isKindOfClass: takes a class.
    let is_font: bool = unsafe { msg_send![any(&*font), isKindOfClass: class] };
    assert!(is_font);
    // SAFETY: pointSize takes nothing.
    let size: f64 = unsafe { msg_send![any(&*font), pointSize] };
    assert_eq!(size, 24.0);
    // AppKit's fonts one test thread at a time (see `common`).
    let system = {
        let _one = appkit_fonts();
        objc2_app_kit::NSFont::systemFontOfSize(13.0)
    };
    let as_ct: &CTFont = unsafe { &*(Retained::as_ptr(&system) as *const CTFont) };
    assert_eq!(CFGetTypeID(Some(as_ct)), CTFont::type_id());
    assert_eq!(unsafe { as_ct.size() }, 13.0);
    // The same object both ways: a descriptor too.
    let descriptor = system.fontDescriptor();
    let as_cf: &CFType = unsafe { &*(Retained::as_ptr(&descriptor) as *const CFType) };
    assert_eq!(CFGetTypeID(Some(as_cf)), CTFontDescriptor::type_id());
    let from_ct = unsafe { as_ct.font_descriptor() };
    let back: &objc2_app_kit::NSFontDescriptor = unsafe { &*(&*from_ct as *const CTFontDescriptor).cast() };
    assert_eq!(back.pointSize(), 13.0);
    // A font made through CoreText, used as an NSFont's attribute.
    let text = attributed("x", &[(unsafe { kCTFontAttributeName }, any(&*font))]);
    let measured = objc2_app_kit::NSAttributedStringNSStringDrawing::size(&*text);
    close(measured.width, 14.203125, 1e-3);
}

#[test]
fn metrics() {
    let font = dejavu(24.0);
    // SAFETY: a font.
    unsafe {
        assert_eq!(font.size(), 24.0);
        assert_eq!(font.units_per_em(), 2048);
        assert_eq!(font.glyph_count(), 6253);
        assert_eq!(font.ascent(), 22.27734375);
        assert_eq!(font.descent(), 5.66015625);
        assert_eq!(font.leading(), 0.0);
        assert_eq!(font.cap_height(), 17.6484375);
        assert_eq!(font.x_height(), 13.27734375);
        assert_eq!(font.underline_position(), -0.46875);
        assert_eq!(font.underline_thickness(), 1.0546875);
        assert_eq!(font.slant_angle(), 0.0);
        close_rect(font.bounding_box(), r(-24.4921875, -11.109375, 67.53515625, 40.6875), 1e-9);
        assert_eq!(font.matrix(), CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 });
        assert_eq!(font.symbolic_traits().0, 0);
        assert_eq!(font.string_encoding(), 0);
        assert_eq!(font.post_script_name().to_string(), "DejaVuSans");
        assert_eq!(font.family_name().to_string(), "DejaVu Sans");
        assert_eq!(font.full_name().to_string(), "DejaVu Sans");
        assert_eq!(font.display_name().to_string(), "DejaVu Sans");
        assert_eq!(font.name(kCTFontSubFamilyNameKey).unwrap().to_string(), "Book");
        assert_eq!(font.name(kCTFontVersionNameKey).unwrap().to_string(), "Version 2.37");
        assert_eq!(font.name(kCTFontPostScriptNameKey).unwrap().to_string(), "DejaVuSans");
        assert!(font.name(kCTFontDesignerNameKey).is_none());
        // Traits: all zero for DejaVu Sans.
        let traits = font.traits();
        let traits: &NSDictionary<NSString, AnyObject> = &*(&*traits as *const CFDictionary).cast();
        for key in [kCTFontSymbolicTrait, kCTFontWeightTrait, kCTFontWidthTrait, kCTFontSlantTrait] {
            let v = traits.objectForKey(ns(key)).expect("a trait");
            let v: f64 = msg_send![&*v, doubleValue];
            assert_eq!(v, 0.0, "{key}");
        }
        // The descriptor names the font and its size.
        let attrs = font.font_descriptor().attributes();
        let attrs: &NSDictionary<NSString, AnyObject> = &*(&*attrs as *const CFDictionary).cast();
        let name = attrs.objectForKey(ns(kCTFontNameAttribute)).unwrap();
        assert_eq!(name.downcast::<NSString>().unwrap().to_string(), "DejaVuSans");
        let size: f64 = msg_send![&*attrs.objectForKey(ns(kCTFontSizeAttribute)).unwrap(), doubleValue];
        assert_eq!(size, 24.0);
        assert!(font.has_table(u32::from_be_bytes(*b"glyf")));
        assert!(!font.has_table(u32::from_be_bytes(*b"CFF ")));
        assert!(font.feature_settings().is_none());
        assert!(font.variation_axes().is_none());
        // Size 0 is 12 points; a copy at another size is another font, at
        // the same size the font itself.
        let default = CTFont::with_graphics_font(&graphics_font(), 0.0, std::ptr::null(), None);
        assert_eq!(default.size(), 12.0);
        assert_eq!(default.ascent(), 11.138671875);
        let bigger = font.copy_with_attributes(36.0, std::ptr::null(), None);
        assert_eq!(bigger.size(), 36.0);
        assert_eq!(font.copy_with_attributes(0.0, std::ptr::null(), None).size(), 24.0);
        let same = font.copy_with_attributes(24.0, std::ptr::null(), None);
        assert!(std::ptr::eq(&*same, &*font));
        // A font file has one face: no bold one.
        let bold = CTFontSymbolicTraits::TraitBold;
        assert!(font.copy_with_symbolic_traits(0.0, std::ptr::null(), bold, bold).is_none());
        let back = font.graphics_font(std::ptr::null_mut());
        assert_eq!(CGFont::post_script_name(Some(&back)).unwrap().to_string(), "DejaVuSans");
        // Without heights in `OS/2`, CoreGraphics finds the same ones:
        // halfway between H's and O's tops, and x's and o's.
        let file = graphics_font();
        assert_eq!(CGFont::cap_height(Some(&file)), 1506);
        assert_eq!(CGFont::x_height(Some(&file)), 1133);
        // And an AppKit font's metrics are the same numbers.
        let ns: &objc2_app_kit::NSFont = &*(&*font as *const CTFont).cast();
        assert_eq!((ns.ascender(), ns.descender()), (font.ascent(), -font.descent()));
        assert_eq!((ns.capHeight(), ns.xHeight()), (font.cap_height(), font.x_height()));
    }
}

#[test]
fn glyphs_and_advances() {
    let font = dejavu(24.0);
    assert_eq!(glyphs_of(&font, "AVfi x"), (vec![36, 57, 73, 76, 3, 91], true));
    // A pair's glyph is in its first unit; missing characters fail.
    assert_eq!(glyphs_of(&font, "A\u{1F600}B"), (vec![36, 5857, 0, 37], true));
    assert_eq!(glyphs_of(&font, "A\u{E000}B"), (vec![36, 0, 37], false));
    let (glyphs, _) = glyphs_of(&font, "AVfi x");
    let g = NonNull::new(glyphs.as_ptr().cast_mut()).unwrap();
    let n = glyphs.len() as CFIndex;
    let mut advances = vec![CGSize::ZERO; glyphs.len()];
    let mut rects = vec![CGRect::ZERO; glyphs.len()];
    // SAFETY: buffers as long as the glyphs.
    unsafe {
        let total = font.advances_for_glyphs(CTFontOrientation::Horizontal, g, advances.as_mut_ptr(), n);
        assert_eq!(total, 69.78515625);
        let widths: Vec<f64> = advances.iter().map(|a| a.width).collect();
        assert_eq!(widths, [16.41796875, 16.41796875, 8.44921875, 6.66796875, 7.62890625, 14.203125]);
        assert!(advances.iter().all(|a| a.height == 0.0));
        assert_eq!(font.advances_for_glyphs(CTFontOrientation::Default, g, std::ptr::null_mut(), n), total);
        let total = font.advances_for_glyphs(CTFontOrientation::Vertical, g, advances.as_mut_ptr(), n);
        assert_eq!(total, 127.62890625);
        let union = font.bounding_rects_for_glyphs(CTFontOrientation::Horizontal, g, rects.as_mut_ptr(), n);
        close_rect(union, r(0.1875, 0.0, 16.03125, 18.234375), 1e-9);
        close_rect(rects[0], r(0.1875, 0.0, 16.03125, 17.49609375), 1e-9);
        close_rect(rects[2], r(0.55078125, 0.0, 8.35546875, 18.234375), 1e-9);
        // The space has nothing to draw: an empty box, left out of the union.
        close_rect(rects[4], CGRect::ZERO, 0.0);
        let union = font.bounding_rects_for_glyphs(CTFontOrientation::Vertical, g, rects.as_mut_ptr(), n);
        close_rect(union, r(0.0, -8.015625, 18.234375, 16.03125), 1e-9);
        close_rect(rects[0], r(0.73828125, -8.015625, 17.49609375, 16.03125), 1e-9);
        // Turned a quarter: across from the glyph's left edge less half its
        // advance (glyphs that aren't symmetric show it).
        let (asym, _) = glyphs_of(&font, "gj,");
        let mut turned = [CGRect::ZERO; 3];
        let asym_g = NonNull::new(asym.as_ptr().cast_mut()).unwrap();
        font.bounding_rects_for_glyphs(CTFontOrientation::Vertical, asym_g, turned.as_mut_ptr(), 3);
        let per_unit = 24.0 / 2048.0;
        let ys: Vec<f64> = turned.iter().map(|t| (t.origin.y / per_unit).round()).collect();
        assert_eq!(ys, [-537.0, -321.0, -167.0]);
        let mut translations = vec![CGSize::ZERO; glyphs.len()];
        font.vertical_translations_for_glyphs(g, NonNull::new(translations.as_mut_ptr()).unwrap(), n);
        assert_eq!((translations[0].width, translations[0].height), (-8.203125, -18.234375));
        assert_eq!((translations[4].width, translations[4].height), (0.0, 0.0));
        let union = font.optical_bounds_for_glyphs(g, rects.as_mut_ptr(), n, 0);
        close_rect(union, r(0.0, -5.66015625, 16.41796875, 27.9375), 1e-9);
        // A glyph the font doesn't have: nothing; glyph 0 is .notdef.
        let bad = [60000u16, 0];
        let bad_g = NonNull::new(bad.as_ptr().cast_mut()).unwrap();
        let union = font.bounding_rects_for_glyphs(CTFontOrientation::Horizontal, bad_g, rects.as_mut_ptr(), 2);
        close_rect(rects[0], CGRect::ZERO, 0.0);
        close_rect(union, r(1.1953125, -4.2421875, 12.0, 21.1640625), 1e-9);
        assert_eq!(
            font.advances_for_glyphs(CTFontOrientation::Horizontal, bad_g, advances.as_mut_ptr(), 2),
            14.40234375
        );
        assert_eq!(advances[0].width, 0.0);
        // Names.
        assert_eq!(font.name_for_glyph(glyphs[2]).unwrap().to_string(), "f");
        assert_eq!(font.glyph_with_name(&CFString::from_str("fi")), 5042);
    }
}

#[test]
fn glyph_paths() {
    let font = dejavu(24.0);
    let (glyphs, _) = glyphs_of(&font, "A ");
    // SAFETY: glyphs of the font, and a transform.
    unsafe {
        let path = font.path_for_glyph(glyphs[0], std::ptr::null()).expect("an outline");
        close_rect(CGPath::bounding_box(Some(&path)), r(0.1875, 0.0, 16.03125, 17.49609375), 1e-9);
        close_rect(CGPath::path_bounding_box(Some(&path)), r(0.1875, 0.0, 16.03125, 17.49609375), 1e-9);
        let flip = CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: -1.0, tx: 10.0, ty: 20.0 };
        let path = font.path_for_glyph(glyphs[0], &flip).expect("an outline");
        close_rect(CGPath::path_bounding_box(Some(&path)), r(10.1875, 2.50390625, 16.03125, 17.49609375), 1e-9);
        assert!(font.path_for_glyph(glyphs[1], std::ptr::null()).is_none());
    }
}

fn feature(kind: i64, selector: i64) -> Retained<NSDictionary<NSString, AnyObject>> {
    // SAFETY: the constants are strings.
    let keys = unsafe { [ns(kCTFontFeatureTypeIdentifierKey), ns(kCTFontFeatureSelectorIdentifierKey)] };
    let (k, s) = (NSNumber::new_i64(kind), NSNumber::new_i64(selector));
    NSDictionary::from_slices(&keys, &[any(&*k), any(&*s)])
}

fn descriptor_with_features(settings: &[Retained<NSDictionary<NSString, AnyObject>>]) -> CFRetained<CTFontDescriptor> {
    let array = NSArray::from_retained_slice(settings);
    // SAFETY: the constant is a string.
    let dict = NSDictionary::from_slices(&[ns(unsafe { kCTFontFeatureSettingsAttribute })], &[any(&*array)]);
    // SAFETY: a dictionary of attributes.
    unsafe { CTFontDescriptor::with_attributes(&*(Retained::as_ptr(&dict) as *const CFDictionary)) }
}

#[test]
fn feature_settings() {
    let font = dejavu(24.0);
    let ligature = runs(&line("fi", &font, &[]));
    assert_eq!(ligature[0].glyphs, [5042]);
    assert_eq!(ligature[0].indices, [0]);
    let no_common = descriptor_with_features(&[feature(kLigaturesType as i64, kCommonLigaturesOffSelector as i64)]);
    // SAFETY: a font and a descriptor.
    unsafe {
        let plain = font.copy_with_attributes(0.0, std::ptr::null(), Some(&no_common));
        assert_eq!(plain.size(), 24.0);
        let fi = runs(&line("fi", &plain, &[]));
        assert_eq!(fi[0].glyphs, [73, 76]);
        close_all(&fi[0].positions.iter().map(|p| p.0).collect::<Vec<_>>(), &[0.0, 8.44921875]);
        // The settings come back as given.
        let settings = plain.feature_settings().expect("settings");
        let settings: &NSArray<AnyObject> = &*(&*settings as *const objc2_core_foundation::CFArray).cast();
        assert_eq!(settings.count(), 1);
        let item = settings.objectAtIndex(0).downcast::<NSDictionary>().unwrap();
        let item: &NSDictionary<NSString, AnyObject> = &*(Retained::as_ptr(&item)).cast();
        let kind: i64 = msg_send![&*item.objectForKey(ns(kCTFontFeatureTypeIdentifierKey)).unwrap(), longLongValue];
        let selector: i64 =
            msg_send![&*item.objectForKey(ns(kCTFontFeatureSelectorIdentifierKey)).unwrap(), longLongValue];
        assert_eq!((kind, selector), (1, 3));
        // The three settings together, and a font made from a file with
        // them.
        let all = descriptor_with_features(&[
            feature(kLigaturesType as i64, kCommonLigaturesOffSelector as i64),
            feature(kContextualAlternatesType as i64, kContextualAlternatesOffSelector as i64),
            feature(kLigaturesType as i64, kRareLigaturesOffSelector as i64),
        ]);
        let made = CTFont::with_graphics_font(&graphics_font(), 24.0, std::ptr::null(), Some(&all));
        assert_eq!(runs(&line("fi", &made, &[]))[0].glyphs, [73, 76]);
        let copied = font.copy_with_attributes(0.0, std::ptr::null(), Some(&all));
        assert_eq!(runs(&line("fi", &copied, &[]))[0].glyphs, [73, 76]);
        // A descriptor with a feature added, by name and size.
        let helvetica = CTFontDescriptor::with_name_and_size(&CFString::from_str("Helvetica"), 12.0);
        let one = NSNumber::new_i64(1);
        let three = NSNumber::new_i64(3);
        let with = helvetica.copy_with_feature(
            &*(Retained::as_ptr(&one) as *const objc2_core_foundation::CFNumber),
            &*(Retained::as_ptr(&three) as *const objc2_core_foundation::CFNumber),
        );
        let attrs = with.attributes();
        let attrs: &NSDictionary<NSString, AnyObject> = &*(&*attrs as *const CFDictionary).cast();
        assert!(attrs.objectForKey(ns(kCTFontFeatureSettingsAttribute)).is_some());
        assert_eq!(
            attrs.objectForKey(ns(kCTFontNameAttribute)).unwrap().downcast::<NSString>().unwrap().to_string(),
            "Helvetica"
        );
        let from = CTFont::with_font_descriptor(&with, 0.0, std::ptr::null());
        assert_eq!(from.size(), 12.0);
        assert!(from.feature_settings().is_some());
        // A variation is kept in the attributes; a face without the axis
        // is as it was.
        let wght = NSNumber::new_u32(u32::from_be_bytes(*b"wght"));
        let described = font.font_descriptor();
        let heavy =
            described.copy_with_variation(&*(Retained::as_ptr(&wght) as *const objc2_core_foundation::CFNumber), 700.0);
        let attrs = heavy.attributes();
        let attrs: &NSDictionary<NSString, AnyObject> = &*(&*attrs as *const CFDictionary).cast();
        assert!(attrs.objectForKey(ns(kCTFontVariationAttribute)).is_some());
        let same = font.copy_with_attributes(0.0, std::ptr::null(), Some(&heavy));
        assert_eq!(same.ascent(), font.ascent());
        assert!(same.variation().is_none());
    }
}

#[test]
fn line_of_text() {
    let font = dejavu(24.0);
    let l = line("AVfi Hello ", &font, &[]);
    let (w, a, d, lead) = typographic(&l);
    close(w, 122.5078125, 1e-3);
    assert_eq!((a, d, lead), (22.27734375, 5.66015625, 0.0));
    // SAFETY: a line.
    unsafe {
        assert_eq!(l.glyph_count(), 10);
        assert_eq!(l.string_range(), CFRange::new(0, 11));
        close(l.trailing_whitespace_width(), 7.62890625, 1e-3);
        close_rect(l.bounds_with_options(CTLineBoundsOptions(0)), r(0.0, -5.66015625, 122.5078125, 27.9375), 1e-3);
        close_rect(
            l.bounds_with_options(CTLineBoundsOptions::ExcludeTypographicLeading),
            r(0.0, -5.66015625, 122.5078125, 27.9375),
            1e-3,
        );
        close_rect(
            l.bounds_with_options(CTLineBoundsOptions::UseOpticalBounds),
            r(0.0, -5.66015625, 114.87890625, 27.9375),
            1e-3,
        );
        let glyph_bounds = r(0.1875, -0.33984375, 113.37890625, 18.57421875);
        close_rect(l.bounds_with_options(CTLineBoundsOptions::UseGlyphPathBounds), glyph_bounds, 1e-3);
        close_rect(l.image_bounds(None), glyph_bounds, 1e-3);
    }
    let run = &runs(&l)[0];
    assert_eq!(run.glyphs, [36, 57, 5042, 3, 43, 72, 79, 79, 82, 3]);
    assert_eq!(run.indices, [0, 1, 2, 4, 5, 6, 7, 8, 9, 10]);
    assert_eq!(run.range, CFRange::new(0, 11));
    assert_eq!(run.status, CTRunStatus::NoStatus);
    close_all(
        &run.positions.iter().map(|p| p.0).collect::<Vec<_>>(),
        &[
            0.0,
            14.8828125,
            31.30078125,
            46.41796875,
            54.046875,
            72.09375,
            86.859375,
            93.52734375,
            100.1953125,
            114.87890625,
        ],
    );
    assert!(run.positions.iter().all(|p| p.1 == 0.0));
    close_all(
        &run.advances,
        &[
            14.8828125,
            16.41796875,
            15.1171875,
            7.62890625,
            18.046875,
            14.765625,
            6.66796875,
            6.66796875,
            14.68359375,
            7.62890625,
        ],
    );
}

#[test]
fn ligatures_reach_their_characters() {
    let font = dejavu(24.0);
    // A ligature ending the run: its range covers both characters.
    let fi = runs(&line("fi", &font, &[]));
    assert_eq!((fi[0].glyphs.clone(), fi[0].range), (vec![5042], CFRange::new(0, 2)));
    // And with another run after it (the x colored).
    let red = CGColor::new_srgb(1.0, 0.0, 0.0, 1.0);
    let s = attributed("fix", &[(unsafe { kCTFontAttributeName }, any(&*font))]);
    let m: Retained<NSMutableAttributedString> = unsafe { msg_send![&*s, mutableCopy] };
    // SAFETY: the constant is a string; a range of the string.
    unsafe { m.addAttribute_value_range(ns(kCTForegroundColorAttributeName), any(&*red), NSRange::new(2, 1)) };
    let l = unsafe { CTLine::with_attributed_string(cf(&m)) };
    let rs = runs(&l);
    assert_eq!(rs.len(), 2);
    assert_eq!((rs[0].range, rs[1].range), (CFRange::new(0, 2), CFRange::new(2, 1)));
}

#[test]
fn run_attributes_are_the_strings() {
    let font = dejavu(24.0);
    // SAFETY: the constant is a string.
    let string = attributed("xy", &[(unsafe { kCTFontAttributeName }, any(&*font))]);
    // SAFETY: an attributed string.
    let l = unsafe { CTLine::with_attributed_string(cf(&string)) };
    let run = &runs(&l)[0];
    let mut range = NSRange::new(0, 0);
    // SAFETY: an index in the string.
    let dict: *const AnyObject = unsafe { msg_send![&*string, attributesAtIndex: 0usize, effectiveRange: &mut range] };
    assert!(std::ptr::eq(dict.cast::<u8>(), (&*run.attributes as *const CFDictionary).cast::<u8>()));
    // Text without a font is in Helvetica 12, as CoreText has it (the
    // interface font on Linux): one run, whose attributes name its font.
    let bare = attributed("Hi", &[]);
    // SAFETY: an attributed string.
    let l = unsafe { CTLine::with_attributed_string(cf(&bare)) };
    let run = &runs(&l)[0];
    let attrs: &NSDictionary<NSString, AnyObject> = unsafe { &*(&*run.attributes as *const CFDictionary).cast() };
    assert_eq!(attrs.count(), 1);
    let f = attrs.objectForKey(ns(unsafe { kCTFontAttributeName })).expect("a font");
    let f: &CTFont = unsafe { &*(Retained::as_ptr(&f) as *const CTFont) };
    assert_eq!(unsafe { f.size() }, 12.0);
}

#[test]
fn separators_tabs_and_empty_lines() {
    let font = dejavu(24.0);
    let empty = line("", &font, &[]);
    assert_eq!(typographic(&empty), (0.0, 0.0, 0.0, 0.0));
    assert_eq!(unsafe { empty.glyph_count() }, 0);
    // A newline is a zero-width glyph (the space's) in the line.
    let newline = line("a\nb", &font, &[]);
    let run = &runs(&newline)[0];
    assert_eq!(run.glyphs, [68, 3, 69]);
    close_all(&run.advances, &[14.70703125, 0.0, 15.234375]);
    close(typographic(&newline).0, 29.94140625, 1e-3);
    let alone = line("\n", &font, &[]);
    let (w, a, d, _) = typographic(&alone);
    assert_eq!((w, a, d), (0.0, 22.27734375, 5.66015625));
    // Nothing after a newline moves.
    let next = line("ab\ncd", &font, &[]);
    close_all(
        &runs(&next)[0].positions.iter().map(|p| p.0).collect::<Vec<_>>(),
        &[0.0, 14.70703125, 29.94140625, 29.94140625, 43.13671875],
    );
    // Line and paragraph separators are the font's own glyphs for them,
    // taking no room.
    let para = line("ab\u{2029}cd", &font, &[]);
    let run = &runs(&para)[0];
    assert_eq!(run.glyphs, [68, 69, 2828, 70, 71]);
    assert_eq!(run.indices, [0, 1, 2, 3, 4]);
    close_all(&run.advances, &[14.70703125, 15.234375, 0.0, 13.1953125, 15.234375]);
    let ending = line("ab\u{2028}", &font, &[]);
    let run = &runs(&ending)[0];
    assert_eq!((run.glyphs.clone(), run.range), (vec![68, 69, 2827], CFRange::new(0, 3)));
    // A control character missing from the font is a space taking no room.
    let nel = line("a\u{85}b", &font, &[]);
    let run = &runs(&nel)[0];
    assert_eq!(run.glyphs, [68, 3, 69]);
    close_all(&run.advances, &[14.70703125, 0.0, 15.234375]);
    // An empty line has no character for a position.
    assert_eq!(unsafe { empty.string_index_for_position(CGPoint::new(5.0, 0.0)) }, -1);
    // A tab reaches the next stop, 28 points on.
    let tab = line("a\tb", &font, &[]);
    let run = &runs(&tab)[0];
    assert_eq!(run.glyphs, [68, 3, 69]);
    close(run.positions[2].0, 28.0, 1e-3);
    // Spaces are all trailing whitespace.
    let spaces = line("   ", &font, &[]);
    close(unsafe { spaces.trailing_whitespace_width() }, 22.88671875, 1e-3);
}

#[test]
fn kerning_and_tracking() {
    let font = dejavu(24.0);
    let av = runs(&line("AV", &font, &[]));
    close_all(&av[0].advances, &[14.8828125, 16.41796875]);
    let two = NSNumber::new_f64(2.0);
    // SAFETY: the constants are strings.
    let kerned = runs(&line("AV", &font, &[(unsafe { kCTKernAttributeName }, any(&*two))]));
    close_all(&kerned[0].advances, &[16.8828125, 18.41796875]);
    let zero = NSNumber::new_f64(0.0);
    // Kerning 0 turns the font's kerning off.
    let unkerned = runs(&line("AV", &font, &[(unsafe { kCTKernAttributeName }, any(&*zero))]));
    close_all(&unkerned[0].advances, &[16.41796875, 16.41796875]);
    let three = NSNumber::new_f64(3.0);
    let tracked = line("Hx", &font, &[(unsafe { kCTTrackingAttributeName }, any(&*three))]);
    close_all(&runs(&tracked)[0].advances, &[21.046875, 17.203125]);
    close(unsafe { tracked.trailing_whitespace_width() }, 3.0, 1e-3);
    // A kern of 0 keeps kerning off beside tracking.
    let both = line(
        "AV",
        &font,
        &[(unsafe { kCTKernAttributeName }, any(&*zero)), (unsafe { kCTTrackingAttributeName }, any(&*three))],
    );
    close_all(&runs(&both)[0].advances, &[19.41796875, 19.41796875]);
    // A baseline offset raises the glyphs and the line's ascent.
    let five = NSNumber::new_f64(5.0);
    let raised = line("Hx", &font, &[(unsafe { kCTBaselineOffsetAttributeName }, any(&*five))]);
    let (_, a, d, _) = typographic(&raised);
    assert_eq!((a, d), (27.27734375, 0.66015625));
    assert!(runs(&raised)[0].positions.iter().all(|p| p.1 == 5.0));
}

#[test]
fn runs_of_two_sizes() {
    let big = dejavu(24.0);
    let small = dejavu(12.0);
    // SAFETY: the constant is a string; a range of the string.
    let string = attributed("AB", &[(unsafe { kCTFontAttributeName }, any(&*big))]);
    let m: Retained<NSMutableAttributedString> = unsafe { msg_send![&*string, mutableCopy] };
    unsafe { m.addAttribute_value_range(ns(kCTFontAttributeName), any(&*small), NSRange::new(1, 1)) };
    let l = unsafe { CTLine::with_attributed_string(cf(&m)) };
    let (w, a, d, _) = typographic(&l);
    close(w, 24.650390625, 1e-3);
    assert_eq!((a, d), (22.27734375, 5.66015625));
    let rs = runs(&l);
    assert_eq!(rs.len(), 2);
    assert_eq!((rs[0].range, rs[1].range), (CFRange::new(0, 1), CFRange::new(1, 1)));
    close(rs[1].positions[0].0, 16.41796875, 1e-3);
    let run: &CTRun = unsafe { &*(l.glyph_runs().value_at_index(1) as *const CTRun) };
    let (mut ra, mut rd, mut rl) = (0.0, 0.0, 0.0);
    let rw = unsafe { run.typographic_bounds(CFRange::new(0, 0), &mut ra, &mut rd, &mut rl) };
    close(rw, 8.232421875, 1e-3);
    assert_eq!((ra, rd, rl), (11.138671875, 2.830078125, 0.0));
}

#[test]
fn right_to_left_runs() {
    let font = dejavu(24.0);
    let l = line("ab \u{5D0}\u{5D1} cd", &font, &[]);
    let rs = runs(&l);
    assert_eq!(rs.len(), 3);
    assert_eq!(rs[1].status, CTRunStatus::RightToLeft);
    assert_eq!(rs[1].glyphs, [1320, 1319]);
    assert_eq!(rs[1].indices, [4, 3]);
    assert_eq!(rs[1].range, CFRange::new(3, 2));
    close(rs[1].positions[0].0, 37.5703125, 1e-3);
    close(typographic(&l).0, 103.546875, 1e-3);
}

#[test]
fn carets() {
    let font = dejavu(24.0);
    let l = line("AVfi x", &font, &[]);
    // Between the kerned A and V the caret is halfway into the kerning; the
    // ligature's characters share it evenly.
    let want = [0.0, 15.650390625, 31.30078125, 38.859375, 46.41796875, 54.046875, 68.25, 68.25];
    for (i, &x) in want.iter().enumerate() {
        let mut secondary = 0.0;
        // SAFETY: a float to write.
        let o = unsafe { l.offset_for_string_index(i as CFIndex, &mut secondary) };
        close(o, x, 1e-3);
        close(secondary, x, 1e-3);
    }
    for (x, index) in [(-5.0, 0), (0.0, 0), (5.0, 0), (10.0, 1), (16.0, 1), (20.0, 1), (40.0, 3), (60.0, 5), (200.0, 6)]
    {
        assert_eq!(unsafe { l.string_index_for_position(CGPoint::new(x, 0.0)) }, index, "at {x}");
    }
    for (flush, offset) in [(0.0, 0.0), (0.5, 65.875), (1.0, 131.75)] {
        close(unsafe { l.pen_offset_for_flush(flush, 200.0) }, offset, 1e-3);
    }
    // A line wider than the width is offset back.
    close(unsafe { l.pen_offset_for_flush(1.0, 50.0) }, -18.25, 1e-3);
    // Edges left to right: leading then trailing, each with the index of
    // its character.
    assert_eq!(
        caret_edges(&l, usize::MAX),
        [
            (0, true, 0.0),
            (0, false, 15.650390625),
            (1, true, 15.650390625),
            (1, false, 31.30078125),
            (2, true, 31.30078125),
            (2, false, 38.859375),
            (3, true, 38.859375),
            (3, false, 46.41796875),
            (4, true, 46.41796875),
            (4, false, 54.046875),
            (5, true, 54.046875),
            (5, false, 68.25),
        ]
        .map(|(i, lead, x)| (i, lead, (x * 1000.0f64).round() as i64))
    );
    // Stopping ends it.
    assert_eq!(caret_edges(&l, 3).len(), 3);
    // An emoji the font has: its own glyph, in the one run. An index inside
    // a pair counts as after the character; the trailing edge has the
    // pair's second unit.
    let pair = line("a\u{1F600}b", &font, &[]);
    assert_eq!(runs(&pair).len(), 1);
    assert_eq!(glyphs_in(&pair), [68, 5857, 69]);
    let mut secondary = 0.0;
    close(unsafe { pair.offset_for_string_index(2, &mut secondary) }, 39.7265625, 1e-3);
    close(secondary, 39.7265625, 1e-3);
    let edges: Vec<(CFIndex, bool)> = caret_edges(&pair, usize::MAX).iter().map(|e| (e.0, e.1)).collect();
    assert_eq!(edges, [(0, true), (0, false), (1, true), (2, false), (3, true), (3, false)]);
}

/// Each caret edge `CTLineEnumerateCaretOffsets` gives, x in thousandths,
/// stopping after `stop` of them.
fn caret_edges(l: &CTLine, stop: usize) -> Vec<(CFIndex, bool, i64)> {
    let seen = std::cell::RefCell::new(Vec::new());
    // Blocks can't take C's bool as objc2 encodes them: bytes, which have
    // its layout.
    let block = block2::RcBlock::new(|offset: f64, index: CFIndex, leading: u8, stop_at: NonNull<u8>| {
        seen.borrow_mut().push((index, leading != 0, (offset * 1000.0).round() as i64));
        if seen.borrow().len() >= stop {
            // SAFETY: CoreText's flag to write.
            unsafe { stop_at.as_ptr().write(1) };
        }
    });
    let block: &ByteCaret<'_> = &block;
    // SAFETY: bytes and bools have the same layout.
    let block: &BoolCaret<'_> = unsafe { std::mem::transmute(block) };
    // SAFETY: a line and a block.
    unsafe { l.enumerate_caret_offsets(block) };
    seen.borrow().clone()
}

/// The caret block as block2 can make it, and as CoreText takes it.
type ByteCaret<'a> = block2::DynBlock<dyn Fn(f64, CFIndex, u8, NonNull<u8>) + 'a>;
type BoolCaret<'a> = block2::DynBlock<dyn Fn(f64, CFIndex, bool, NonNull<bool>) + 'a>;

#[test]
fn right_to_left_carets() {
    let font = dejavu(24.0);
    // Where the direction changes, the character before an index and its
    // own have edges apart: the primary offset is the first's, the
    // secondary the second's.
    let mixed = line("ab \u{5D0}\u{5D1} cd", &font, &[]);
    let want = [
        (-1, 0.0, 0.0),
        (0, 0.0, 0.0),
        (1, 14.70703125, 14.70703125),
        (2, 29.94140625, 29.94140625),
        (3, 37.5703125, 67.48828125),
        (4, 51.4453125, 51.4453125),
        (5, 37.5703125, 67.48828125),
        (6, 75.1171875, 75.1171875),
        (8, 103.546875, 103.546875),
        (9, 103.546875, 103.546875),
    ];
    for (i, primary, secondary) in want {
        let mut other = 0.0;
        // SAFETY: a float to write.
        let o = unsafe { mixed.offset_for_string_index(i, &mut other) };
        close(o, primary, 1e-3);
        close(other, secondary, 1e-3);
    }
    // A position finds the character under it and the edge of that half.
    for (x, index) in
        [(-10.0, 0), (20.0, 1), (35.0, 3), (40.0, 5), (45.0, 4), (55.0, 4), (60.0, 3), (70.0, 5), (200.0, 8)]
    {
        assert_eq!(unsafe { mixed.string_index_for_position(CGPoint::new(x, 0.0)) }, index, "at {x}");
    }
    let edges: Vec<(CFIndex, bool)> = caret_edges(&mixed, usize::MAX).iter().map(|e| (e.0, e.1)).collect();
    assert_eq!(&edges[4..10], [(2, true), (2, false), (4, false), (4, true), (3, false), (3, true)]);
    // All right to left: the end is on the left.
    let rtl = line("\u{5D0}\u{5D1}\u{5D2}", &font, &[]);
    for (i, x) in [(-1, 39.80859375), (0, 39.80859375), (1, 23.765625), (2, 9.890625), (3, 0.0), (4, 0.0)] {
        close(unsafe { rtl.offset_for_string_index(i, std::ptr::null_mut()) }, x, 1e-3);
    }
    for (x, index) in [(-10.0, 3), (5.0, 2), (20.0, 1), (35.0, 0), (200.0, 0)] {
        assert_eq!(unsafe { rtl.string_index_for_position(CGPoint::new(x, 0.0)) }, index, "at {x}");
    }
    let edges: Vec<(CFIndex, bool)> = caret_edges(&rtl, usize::MAX).iter().map(|e| (e.0, e.1)).collect();
    assert_eq!(edges, [(2, false), (2, true), (1, false), (1, true), (0, false), (0, true)]);
}

#[test]
fn truncation_and_justification() {
    let font = dejavu(24.0);
    let long = line("Hello wonderful world", &font, &[]);
    let token = line("\u{2026}", &font, &[]);
    // SAFETY: lines.
    unsafe {
        let end = long.truncated_line(100.0, CTLineTruncationType::End, Some(&token)).unwrap();
        let rs = runs(&end);
        assert_eq!(rs.iter().flat_map(|r| r.glyphs.clone()).collect::<Vec<_>>(), [43, 72, 79, 79, 82, 2825]);
        assert_eq!(rs.last().unwrap().range, CFRange::new(5, 16));
        close(typographic(&end).0, 84.83203125, 1e-3);
        let start = long.truncated_line(100.0, CTLineTruncationType::Start, Some(&token)).unwrap();
        let rs = runs(&start);
        assert_eq!(rs.iter().flat_map(|r| r.glyphs.clone()).collect::<Vec<_>>(), [2825, 90, 82, 85, 79, 71]);
        assert_eq!(rs[0].range, CFRange::new(0, 16));
        close(typographic(&start).0, 90.08203125, 1e-3);
        let middle = long.truncated_line(100.0, CTLineTruncationType::Middle, Some(&token)).unwrap();
        let rs = runs(&middle);
        assert_eq!(rs.iter().flat_map(|r| r.glyphs.clone()).collect::<Vec<_>>(), [43, 72, 2825, 85, 79, 71]);
        assert_eq!(rs[1].range, CFRange::new(2, 16));
        close(typographic(&middle).0, 88.58203125, 1e-3);
        // Half the room for each end.
        let small = dejavu(14.0);
        let long14 = line("Hello wonderful world", &small, &[]);
        let token14 = line("\u{2026}", &small, &[]);
        let cut = long14.truncated_line(134.0, CTLineTruncationType::Middle, Some(&token14)).unwrap();
        let glyphs: Vec<u16> = runs(&cut).iter().flat_map(|r| r.glyphs.clone()).collect();
        assert_eq!(glyphs, [43, 72, 79, 79, 82, 3, 90, 82, 2825, 88, 79, 3, 90, 82, 85, 79, 71]);
        // Wide enough: the line as it is.
        let wide = long.truncated_line(1000.0, CTLineTruncationType::End, Some(&token)).unwrap();
        assert_eq!(wide.glyph_count(), 21);
        let justified = long.justified_line(1.0, 400.0).unwrap();
        assert_eq!(justified.glyph_count(), 21);
        close(typographic(&justified).0, 400.0, 1e-3);
    }
}

/// The glyphs of a line's runs, one after the other.
fn glyphs_in(l: &CTLine) -> Vec<u16> {
    runs(l).iter().flat_map(|r| r.glyphs.clone()).collect()
}

#[test]
fn truncation_edges() {
    let font = dejavu(24.0);
    let long = line("Hello wonderful world", &font, &[]);
    let token = line("\u{2026}", &font, &[]);
    // SAFETY: lines.
    unsafe {
        // No token: nothing stands for what's left out.
        let end = long.truncated_line(100.0, CTLineTruncationType::End, None).unwrap();
        assert_eq!(glyphs_in(&end), [43, 72, 79, 79, 82, 3, 90]);
        assert_eq!(runs(&end)[0].range, CFRange::new(0, 21));
        close(typographic(&end).0, 88.08984375, 1e-3);
        let start = long.truncated_line(100.0, CTLineTruncationType::Start, None).unwrap();
        assert_eq!(runs(&start)[0].indices, [13, 14, 15, 16, 17, 18, 19, 20]);
        assert_eq!(runs(&start)[0].range, CFRange::new(13, 8));
        let middle = long.truncated_line(100.0, CTLineTruncationType::Middle, None).unwrap();
        let rs = runs(&middle);
        assert_eq!((rs[0].range, rs[1].range), (CFRange::new(0, 17), CFRange::new(17, 4)));
        // Narrower than the token: no line; as wide: the token alone.
        assert!(long.truncated_line(5.0, CTLineTruncationType::End, Some(&token)).is_none());
        assert!(long.truncated_line(0.0, CTLineTruncationType::End, Some(&token)).is_none());
        assert!(long.truncated_line(-10.0, CTLineTruncationType::End, Some(&token)).is_none());
        let only = long.truncated_line(30.0, CTLineTruncationType::End, Some(&token)).unwrap();
        assert_eq!(glyphs_in(&only), [2825]);
        assert_eq!(runs(&only)[0].range, CFRange::new(0, 21));
        // Trailing whitespace adds to the room.
        let spaced = line("Hello   ", &font, &[]);
        let cut = spaced.truncated_line(60.0, CTLineTruncationType::End, Some(&token)).unwrap();
        assert_eq!(glyphs_in(&cut), [43, 72, 79, 79, 2825]);
        close(typographic(&cut).0, 70.1484375, 1e-3);
        // Text that fits but for its trailing whitespace loses that.
        let trimmed = spaced.truncated_line(65.0, CTLineTruncationType::End, Some(&token)).unwrap();
        assert_eq!(glyphs_in(&trimmed), [43, 72, 79, 79, 82]);
        assert_eq!(trimmed.string_range(), CFRange::new(0, 8));
        assert_eq!(runs(&trimmed)[0].range, CFRange::new(0, 5));
        // Negative tracking and an empty token.
        let minus = NSNumber::new_f64(-3.0);
        let tight = line("Hello", &font, &[(kCTTrackingAttributeName, any(&*minus))]);
        let empty = line("", &font, &[]);
        let w = typographic(&tight).0;
        for kind in [CTLineTruncationType::End, CTLineTruncationType::Start, CTLineTruncationType::Middle] {
            assert!(tight.truncated_line(w + 1.0, kind, Some(&empty)).is_some());
            assert!(tight.truncated_line(w - 20.0, kind, Some(&empty)).is_some());
        }
    }
}

#[test]
fn justification() {
    let font = dejavu(24.0);
    let long = line("Hello wonderful world", &font, &[]);
    let plain = runs(&long)[0].advances.clone();
    let added = |l: &CTLine| -> Vec<f64> { runs(l)[0].advances.iter().zip(&plain).map(|(a, b)| a - b).collect() };
    // SAFETY: lines.
    unsafe {
        // Spaces first, up to half an em each; then the gaps between
        // letters, and half as much the spaces. The glyph before a space
        // and the last take nothing.
        let small = long.justified_line(1.0, 261.890625 + 20.0).unwrap();
        let d = added(&small);
        close_all(&[d[0], d[4], d[5], d[20]], &[0.0, 0.0, 10.0, 0.0]);
        let full = long.justified_line(1.0, 400.0).unwrap();
        let d = added(&full);
        close(d[0], (138.109375 - 24.0) / 17.0, 1e-3);
        close(d[5], 12.0 + (138.109375 - 24.0) / 34.0, 1e-3);
        close_all(&[d[4], d[20]], &[0.0, 0.0]);
        // Partly.
        let half = long.justified_line(0.5, 400.0).unwrap();
        close(typographic(&half).0, 330.9453125, 1e-3);
        close(added(&half)[5], 12.0 + (69.0546875 - 24.0) / 34.0, 1e-3);
        // Narrower: letters give up to 11/128 em, spaces the rest.
        let narrow = long.justified_line(1.0, 100.0).unwrap();
        close(typographic(&narrow).0, 100.0, 1e-3);
        let d = added(&narrow);
        close(d[0], -2.0625, 1e-3);
        close(d[5], -64.4453125, 1e-3);
        // Partial justification never narrows; a factor of 0 changes
        // nothing.
        assert!(long.justified_line(0.5, 100.0).is_none());
        close(typographic(&long.justified_line(0.0, 100.0).unwrap()).0, 261.890625, 1e-3);
        // Trailing whitespace is dropped.
        let trailing = line("a b  ", &font, &[]);
        let t = trailing.justified_line(1.0, 57.828125).unwrap();
        close_all(&runs(&t)[0].advances, &[14.70703125, 27.88671875, 15.234375, 0.0, 0.0]);
    }
}

#[test]
fn bounds_options() {
    let font = dejavu(24.0);
    // SAFETY: lines.
    unsafe {
        // A baseline offset moves the glyphs, not the fonts' box.
        let five = NSNumber::new_f64(5.0);
        let raised = line("Hi", &font, &[(kCTBaselineOffsetAttributeName, any(&*five))]);
        let box_ = r(0.0, -5.66015625, 24.71484375, 27.9375);
        close_rect(raised.bounds_with_options(CTLineBoundsOptions(0)), box_, 1e-3);
        close_rect(raised.bounds_with_options(CTLineBoundsOptions::ExcludeTypographicShifts), box_, 1e-3);
        assert_eq!(raised.bounds_with_options(CTLineBoundsOptions::UseGlyphPathBounds).origin.y, 5.0);
        // Hanging punctuation: a period and quotes hang, an exclamation
        // mark doesn't.
        let hang = CTLineBoundsOptions::UseHangingPunctuation;
        close_rect(
            line("Hi.  ", &font, &[]).bounds_with_options(hang),
            r(0.0, -5.66015625, 24.71484375, 27.9375),
            1e-3,
        );
        let quoted = line("\u{201c}Hi\u{201d}", &font, &[]).bounds_with_options(hang);
        close_rect(quoted, r(11.6953125, -5.66015625, 24.71484375, 27.9375), 1e-3);
        close(line("Hi!", &font, &[]).bounds_with_options(hang).size.width, 34.3359375, 1e-3);
        let glyphs = line("Hi.", &font, &[]).bounds_with_options(hang | CTLineBoundsOptions::UseGlyphPathBounds);
        close_rect(glyphs, r(2.35546875, 0.0, 20.109375, 18.234375), 1e-3);
        // Optical bounds win over glyph bounds.
        let both = CTLineBoundsOptions::UseOpticalBounds | CTLineBoundsOptions::UseGlyphPathBounds;
        close_rect(line("Hi.  ", &font, &[]).bounds_with_options(both), r(0.0, -5.66015625, 32.34375, 27.9375), 1e-3);
        // Language extents, from the largest font on the line.
        let small = dejavu(12.0);
        let s = attributed("Hi", &[(kCTFontAttributeName, any(&*small))]);
        let m: Retained<NSMutableAttributedString> = msg_send![&*s, mutableCopy];
        m.appendAttributedString(&attributed("Hi", &[(kCTFontAttributeName, any(&*font))]));
        let mixed = CTLine::with_attributed_string(cf(&m));
        let lang = mixed.bounds_with_options(CTLineBoundsOptions::IncludeLanguageExtents);
        let content = typographic(&mixed).0;
        close_rect(lang, r(-4.476576, -12.42969, content + 7.1646, 38.128932), 1e-3);
    }
}

#[test]
fn typesetter_breaks() {
    let font = dejavu(24.0);
    // SAFETY: the constant is a string.
    let string = attributed("Hello wonderful world of text", &[(unsafe { kCTFontAttributeName }, any(&*font))]);
    // SAFETY: an attributed string, and indexes in it.
    unsafe {
        let ts = CTTypesetter::with_attributed_string(cf(&string));
        for (width, line, cluster) in [(10.0, 1, 1), (60.0, 4, 4), (100.0, 6, 7), (150.0, 6, 11), (1000.0, 29, 29)] {
            assert_eq!(ts.suggest_line_break(0, width), line, "line break at {width}");
            assert_eq!(ts.suggest_cluster_break(0, width), cluster, "cluster break at {width}");
        }
        assert_eq!(ts.suggest_line_break(6, 100.0), 7);
        let l = ts.line(CFRange::new(6, 9));
        assert_eq!(l.string_range(), CFRange::new(6, 9));
        let rs = runs(&l);
        assert_eq!(rs[0].glyphs, [90, 82, 81, 71, 72, 85, 73, 88, 79]);
        assert_eq!(rs[0].indices, [6, 7, 8, 9, 10, 11, 12, 13, 14]);
        close(rs[0].positions[0].0, 0.0, 1e-9);
        close(typographic(&l).0, 119.71875, 1e-3);
        // Asked one line after another, the same typesetter breaks a
        // paragraph as new ones do.
        let (mut at, mut fresh) = (0, 0);
        while at < 29 {
            at += ts.suggest_line_break(at, 100.0);
            fresh += CTTypesetter::with_attributed_string(cf(&string)).suggest_line_break(fresh, 100.0);
            assert_eq!(at, fresh);
        }
        assert_eq!(at, 29);
        // An offset moves the tab stops: they're measured from that far
        // before the line.
        let tabbed = attributed("a\tb", &[(kCTFontAttributeName, any(&*font))]);
        let ts = CTTypesetter::with_attributed_string(cf(&tabbed));
        let l = ts.line_with_offset(CFRange::new(0, 0), 10.0);
        close(runs(&l)[0].positions[2].0, 18.0, 1e-3);
    }
}

#[test]
fn frames() {
    let font = dejavu(24.0);
    // SAFETY: the constant is a string.
    let text = attributed(
        "Hello wonderful world of text\nSecond paragraph",
        &[(unsafe { kCTFontAttributeName }, any(&*font))],
    );
    // SAFETY: an attributed string, a path and buffers of the lines' count.
    unsafe {
        let fs = CTFramesetter::with_attributed_string(cf(&text));
        let mut fit = CFRange::new(0, 0);
        let size =
            fs.suggest_frame_size_with_constraints(CFRange::new(0, 0), None, CGSize::new(150.0, f64::MAX), &mut fit);
        close(size.width, 124.76953125, 1e-3);
        assert_eq!(size.height, 168.0);
        assert_eq!(fit, CFRange::new(0, 46));
        let size = fs.suggest_frame_size_with_constraints(CFRange::new(0, 0), None, CGSize::new(150.0, 60.0), &mut fit);
        close(size.width, 119.71875, 1e-3);
        assert_eq!(size.height, 56.0);
        assert_eq!(fit, CFRange::new(0, 16));
        let path = CGPath::with_rect(r(10.0, 10.0, 150.0, 200.0), std::ptr::null());
        let frame = fs.frame(CFRange::new(0, 0), &path, None);
        let lines = frame.lines();
        assert_eq!(lines.count(), 6);
        assert_eq!(frame.string_range(), CFRange::new(0, 46));
        assert_eq!(frame.visible_string_range(), CFRange::new(0, 46));
        let mut origins = vec![CGPoint::ZERO; 6];
        frame.line_origins(CFRange::new(0, 0), NonNull::new(origins.as_mut_ptr()).unwrap());
        let ys: Vec<f64> = origins.iter().map(|p| p.y).collect();
        assert_eq!(ys, [178.0, 150.0, 122.0, 94.0, 66.0, 38.0]);
        assert!(origins.iter().all(|p| p.x == 0.0));
        let ranges: Vec<CFRange> =
            (0..6).map(|i| (*(lines.value_at_index(i) as *const CTLine)).string_range()).collect();
        let want = [(0, 6), (6, 10), (16, 9), (25, 5), (30, 7), (37, 9)].map(|(a, b)| CFRange::new(a, b));
        assert_eq!(ranges, want);
        let small = CGPath::with_rect(r(0.0, 0.0, 150.0, 60.0), std::ptr::null());
        let frame = fs.frame(CFRange::new(0, 0), &small, None);
        assert_eq!(frame.lines().count(), 2);
        assert_eq!(frame.visible_string_range(), CFRange::new(0, 16));
        // The string range is the range asked for, what fits or not.
        assert_eq!(frame.string_range(), CFRange::new(0, 46));
        // A range's lines break as the whole paragraph's do, cut at its end.
        let frame = fs.frame(CFRange::new(6, 20), &small, None);
        assert_eq!(frame.string_range(), CFRange::new(6, 20));
        assert_eq!(frame.visible_string_range(), CFRange::new(6, 19));
        // A centered paragraph.
        let align = CTTextAlignment::Center;
        let setting = CTParagraphStyleSetting {
            spec: CTParagraphStyleSpecifier::Alignment,
            valueSize: std::mem::size_of::<CTTextAlignment>(),
            value: NonNull::from(&align).cast(),
        };
        let style = CTParagraphStyle::new(&setting, 1);
        let mut got = CTTextAlignment::Natural;
        let ok = style.value_for_specifier(
            CTParagraphStyleSpecifier::Alignment,
            std::mem::size_of::<CTTextAlignment>(),
            NonNull::from(&mut got).cast(),
        );
        assert!(ok);
        assert_eq!(got, CTTextAlignment::Center);
        let mut adjustment: f64 = -1.0;
        assert!(style.value_for_specifier(
            CTParagraphStyleSpecifier::LineSpacingAdjustment,
            8,
            NonNull::from(&mut adjustment).cast()
        ));
        assert_eq!(adjustment, 0.0);
        let centered =
            attributed("Hi", &[(kCTFontAttributeName, any(&*font)), (kCTParagraphStyleAttributeName, any(&*style))]);
        let fs = CTFramesetter::with_attributed_string(cf(&centered));
        let frame = fs.frame(CFRange::new(0, 0), &path, None);
        let mut origin = [CGPoint::ZERO];
        frame.line_origins(CFRange::new(0, 1), NonNull::new(origin.as_mut_ptr()).unwrap());
        close(origin[0].x, 62.642578125, 1e-3);
        assert_eq!(origin[0].y, 178.0);
    }
}

#[test]
fn drawing_lines() {
    let font = dejavu(24.0);
    let hxg = line("Hxg", &font, &[]);
    let width = typographic(&hxg).0;
    // At the text position, which moves on by the line's width.
    let c = ctx(120, 60);
    // SAFETY: a line and a context.
    unsafe {
        CGContext::set_text_position(Some(&c), 10.0, 20.0);
        hxg.draw(&c);
    }
    ink_near(&c, [12, 22, 56, 45]);
    assert_eq!(px(&c, 15, 30), [0, 0, 0, 255]);
    let after = CGContext::text_position(Some(&c));
    close(after.x, 10.0 + width, 1e-3);
    assert_eq!(after.y, 20.0);
    // Through the text matrix, and the CTM.
    let c = ctx(120, 60);
    let double = CGAffineTransform { a: 2.0, b: 0.0, c: 0.0, d: 2.0, tx: 0.0, ty: 0.0 };
    CGContext::set_text_matrix(Some(&c), double);
    CGContext::set_text_position(Some(&c), 10.0, 10.0);
    unsafe { hxg.draw(&c) };
    ink_near(&c, [14, 14, 69, 60]);
    let c = ctx(120, 60);
    CGContext::scale_ctm(Some(&c), 2.0, 2.0);
    CGContext::set_text_position(Some(&c), 5.0, 5.0);
    unsafe { hxg.draw(&c) };
    ink_near(&c, [14, 14, 101, 60]);
    // A flipped CTM draws the text upside down, unless the text matrix
    // flips it back.
    let c = ctx(120, 60);
    CGContext::translate_ctm(Some(&c), 0.0, 60.0);
    CGContext::scale_ctm(Some(&c), 1.0, -1.0);
    CGContext::set_text_position(Some(&c), 10.0, 40.0);
    unsafe { hxg.draw(&c) };
    ink_near(&c, [12, 34, 56, 58]);
    let c = ctx(120, 60);
    CGContext::translate_ctm(Some(&c), 0.0, 60.0);
    CGContext::scale_ctm(Some(&c), 1.0, -1.0);
    CGContext::set_text_matrix(Some(&c), CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: -1.0, tx: 0.0, ty: 0.0 });
    CGContext::set_text_position(Some(&c), 10.0, 40.0);
    unsafe { hxg.draw(&c) };
    ink_near(&c, [12, 22, 56, 45]);
    // Image bounds from a context are from its text position.
    let c = ctx(60, 60);
    CGContext::set_text_position(Some(&c), 10.0, 20.0);
    let h = line("H", &font, &[]);
    unsafe { h.draw(&c) };
    let bounds = unsafe { h.image_bounds(Some(&c)) };
    close_rect(bounds, r(30.40234375, 20.0, 13.3359375, 17.49609375), 1e-3);
    // One run's glyphs, at their places from the text position.
    let c = ctx(120, 60);
    CGContext::set_text_position(Some(&c), 10.0, 20.0);
    let run: &CTRun = unsafe { &*(hxg.glyph_runs().value_at_index(0) as *const CTRun) };
    unsafe { run.draw(&c, CFRange::new(1, 1)) };
    ink_near(&c, [28, 26, 42, 40]);
}

#[test]
fn drawing_colors_and_modes() {
    let font = dejavu(24.0);
    let h = |extra: &[(&CFString, &AnyObject)]| line("H", &font, extra);
    // Black whatever the fill color, unless an attribute says otherwise.
    let c = ctx(60, 60);
    CGContext::set_rgb_fill_color(Some(&c), 0.0, 0.0, 1.0, 1.0);
    CGContext::set_text_position(Some(&c), 10.0, 20.0);
    unsafe { h(&[]).draw(&c) };
    assert_eq!(px(&c, 13, 30), [0, 0, 0, 255]);
    let red = CGColor::new_srgb(1.0, 0.0, 0.0, 1.0);
    let c = ctx(60, 60);
    CGContext::set_text_position(Some(&c), 10.0, 20.0);
    unsafe { h(&[(kCTForegroundColorAttributeName, any(&*red))]).draw(&c) };
    assert_eq!(px(&c, 13, 30), [255, 0, 0, 255]);
    let ns_red = objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    // SAFETY: AppKit's constant is a string.
    let ns_color_key: &CFString =
        unsafe { &*(objc2_app_kit::NSForegroundColorAttributeName as *const NSString).cast() };
    let c = ctx(60, 60);
    CGContext::set_text_position(Some(&c), 10.0, 20.0);
    unsafe { h(&[(ns_color_key, any(&*ns_red))]).draw(&c) };
    assert_eq!(px(&c, 13, 30), [255, 0, 0, 255]);
    let yes = NSNumber::new_bool(true);
    let c = ctx(60, 60);
    CGContext::set_rgb_fill_color(Some(&c), 0.0, 1.0, 0.0, 1.0);
    CGContext::set_text_position(Some(&c), 10.0, 20.0);
    unsafe { h(&[(kCTForegroundColorFromContextAttributeName, any(&*yes))]).draw(&c) };
    assert_eq!(px(&c, 13, 30), [0, 255, 0, 255]);
    // Stroked text is hollow; invisible text draws nothing.
    let c = ctx(60, 60);
    CGContext::set_text_drawing_mode(Some(&c), CGTextDrawingMode::Stroke);
    CGContext::set_text_position(Some(&c), 10.0, 20.0);
    unsafe { h(&[]).draw(&c) };
    ink_near(&c, [11, 22, 27, 41]);
    assert_eq!(px(&c, 13, 30)[3], 0);
    let c = ctx(60, 60);
    CGContext::set_text_drawing_mode(Some(&c), CGTextDrawingMode::Invisible);
    CGContext::set_text_position(Some(&c), 10.0, 20.0);
    unsafe { h(&[]).draw(&c) };
    assert!(ink(&c, 0).is_none());
    // A positive stroke width outlines the glyphs.
    let three = NSNumber::new_f64(3.0);
    let c = ctx(60, 40);
    CGContext::set_text_position(Some(&c), 10.0, 10.0);
    unsafe { line("H", &font, &[(kCTStrokeWidthAttributeName, any(&*three))]).draw(&c) };
    ink_near(&c, [11, 12, 27, 31]);
    assert_eq!(px(&c, 13, 20)[3], 0);
}

#[test]
fn drawing_glyphs() {
    let font = dejavu(24.0);
    let (glyphs, _) = glyphs_of(&font, "H");
    let g = NonNull::new(glyphs.as_ptr().cast_mut()).unwrap();
    let draw = |c: &CGContext, x: f64, y: f64| {
        let p = [CGPoint::new(x, y)];
        // SAFETY: one glyph and one position.
        unsafe { font.draw_glyphs(g, NonNull::new(p.as_ptr().cast_mut()).unwrap(), 1, c) };
    };
    // Positions go through the text matrix, its translation included; the
    // fill color is the glyphs'; nothing about the text state changes.
    let c = ctx(120, 60);
    CGContext::set_rgb_fill_color(Some(&c), 0.0, 0.0, 1.0, 1.0);
    draw(&c, 10.0, 20.0);
    ink_near(&c, [12, 22, 26, 40]);
    assert_eq!(px(&c, 13, 30), [0, 0, 255, 255]);
    assert_eq!(CGContext::text_position(Some(&c)), CGPoint::new(0.0, 0.0));
    let c = ctx(120, 60);
    CGContext::set_text_position(Some(&c), 30.0, 0.0);
    draw(&c, 10.0, 20.0);
    ink_near(&c, [42, 22, 56, 40]);
    let c = ctx(120, 60);
    CGContext::set_text_matrix(Some(&c), CGAffineTransform { a: 2.0, b: 0.0, c: 0.0, d: 2.0, tx: 0.0, ty: 0.0 });
    draw(&c, 10.0, 5.0);
    ink_near(&c, [24, 14, 52, 50]);
    // CoreGraphics' own: the context's font and size.
    let c = ctx(60, 40);
    CGContext::set_font(Some(&c), Some(&graphics_font()));
    CGContext::set_font_size(Some(&c), 24.0);
    let p = [CGPoint::new(10.0, 10.0)];
    // SAFETY: one glyph and one position.
    unsafe { CGContext::show_glyphs_at_positions(Some(&c), glyphs.as_ptr(), p.as_ptr(), 1) };
    ink_near(&c, [12, 12, 26, 30]);
    assert_eq!(CGContext::text_position(Some(&c)), CGPoint::new(0.0, 0.0));
    let c = ctx(60, 40);
    CGContext::set_font(Some(&c), Some(&graphics_font()));
    CGContext::set_font_size(Some(&c), 24.0);
    CGContext::set_text_position(Some(&c), 10.0, 10.0);
    // SAFETY: one glyph.
    #[allow(deprecated)]
    unsafe {
        CGContext::show_glyphs(Some(&c), glyphs.as_ptr(), 1)
    };
    ink_near(&c, [12, 12, 26, 30]);
    assert_eq!(CGContext::text_position(Some(&c)), CGPoint::new(28.046875, 10.0));
}

#[test]
fn attributed_strings() {
    // SAFETY: strings and attributed strings made here; ranges inside them.
    unsafe {
        let key = CFString::from_str("K");
        let value = CFString::from_str("v");
        let keys = [&*key];
        let values = [&*value];
        let dict = objc2_core_foundation::CFDictionary::from_slices(&keys, &values);
        let dict: &CFDictionary = &*(&*dict as *const CFDictionary<CFString, CFString>).cast();
        let s = CFAttributedString::new(None, Some(&CFString::from_str("héllo")), Some(dict)).unwrap();
        assert_eq!(s.length(), 5);
        assert_eq!(s.string().unwrap().to_string(), "héllo");
        assert_eq!(CFGetTypeID(Some(&s)), CFAttributedString::type_id());
        let mut range = CFRange::new(0, 0);
        let attrs = s.attributes(2, &mut range).unwrap();
        assert_eq!(range, CFRange::new(0, 5));
        assert_eq!(attrs.count(), 1);
        let got = s.attribute(1, Some(&key), &mut range).unwrap();
        assert_eq!(CFGetTypeID(Some(&*got)), CFString::type_id());
        let m = CFMutableAttributedString::new_copy(None, 0, Some(&s)).unwrap();
        let other = CFString::from_str("w");
        CFMutableAttributedString::set_attribute(Some(&m), CFRange::new(1, 2), Some(&key), Some(&other));
        let mut longest = CFRange::new(0, 0);
        let v = m.attribute_and_longest_effective_range(1, Some(&key), CFRange::new(0, 5), &mut longest).unwrap();
        assert_eq!(longest, CFRange::new(1, 2));
        assert_eq!((*(&*v as *const CFType as *const CFString)).to_string(), "w");
        CFMutableAttributedString::replace_string(Some(&m), CFRange::new(0, 1), Some(&CFString::from_str("J")));
        assert_eq!(m.string().unwrap().to_string(), "Jéllo");
        CFMutableAttributedString::remove_attribute(Some(&m), CFRange::new(0, 5), Some(&key));
        assert!(m.attribute(3, Some(&key), std::ptr::null_mut()).is_none());
        let sub = CFAttributedString::with_substring(None, Some(&s), CFRange::new(1, 3)).unwrap();
        assert_eq!(sub.string().unwrap().to_string(), "éll");
        // An NSAttributedString is a CFAttributedString.
        let ns_string: &NSAttributedString = &*(&*s as *const CFAttributedString).cast();
        assert_eq!(ns_string.string().to_string(), "héllo");
    }
}

#[test]
#[allow(deprecated)] // Registering a CGFont, which the manager still does.
fn font_by_name_and_descriptor() {
    // SAFETY: names and descriptors.
    unsafe {
        let missing = CTFont::with_name(&CFString::from_str("NoSuchFontXYZ"), 0.0, std::ptr::null());
        assert_eq!(missing.size(), 12.0);
        let helvetica = CTFont::with_name(&CFString::from_str("Helvetica"), 0.0, std::ptr::null());
        assert_eq!(missing.post_script_name().to_string(), helvetica.post_script_name().to_string());
        let ui = CTFont::new_ui_font_for_language(CTFontUIFontType::System, 0.0, None).unwrap();
        assert_eq!(ui.size(), 13.0);
        let fixed = CTFont::new_ui_font_for_language(CTFontUIFontType::UserFixedPitch, 0.0, None).unwrap();
        assert_eq!(fixed.size(), 10.0);
        let d = CTFontDescriptor::with_name_and_size(&CFString::from_str("Helvetica"), 12.0);
        assert_eq!(d.attributes().count(), 2);
        let matched = d.matching_font_descriptor(None).expect("a match");
        assert_eq!(matched.attributes().count(), 1);
        assert_eq!(d.matching_font_descriptors(None).unwrap().count(), 1);
        let font = CTFont::with_font_descriptor(&d, 0.0, std::ptr::null());
        assert_eq!(font.size(), 12.0);
        // The font manager's registrations are the process's: they're all
        // here, one after another, rather than in tests running at once.
        // This assumes the Mac running it doesn't have DejaVu Sans
        // installed (macOS turns a second face of a name away).
        //
        // A font file's face that isn't registered matches nothing.
        let file = graphics_font();
        let unregistered = CTFont::with_graphics_font(&file, 12.0, std::ptr::null(), None);
        assert!(unregistered.font_descriptor().matching_font_descriptors(None).is_none());
        // Nor is it found by name, unless the system has it (Linux does).
        let system = CTFont::with_name(&CFString::from_str("DejaVuSans"), 12.0, std::ptr::null());
        if system.post_script_name().to_string() != "DejaVuSans" {
            let by_name = CTFontDescriptor::with_name_and_size(&CFString::from_str("DejaVuSans"), 12.0);
            assert!(by_name.matching_font_descriptor(None).is_none());
        }
        // Registered, it's found by its name; registering it again is
        // fine.
        assert!(CTFontManagerRegisterGraphicsFont(&file, std::ptr::null_mut()));
        let mut error: *mut objc2_core_foundation::CFError = std::ptr::null_mut();
        assert!(CTFontManagerRegisterGraphicsFont(&file, &mut error));
        assert!(error.is_null());
        let named = CTFont::with_name(&CFString::from_str("DejaVuSans"), 20.0, std::ptr::null());
        assert_eq!(named.post_script_name().to_string(), "DejaVuSans");
        assert_eq!(named.ascent(), 1901.0 * 20.0 / 2048.0);
        assert!(CTFontManagerUnregisterGraphicsFont(&file, std::ptr::null_mut()));
        // Unregistering it again fails, and says so (with code -1).
        assert!(!CTFontManagerUnregisterGraphicsFont(&file, &mut error));
        assert_eq!(manager_error(error), -1);
        // By URL: registering twice fails the second time, and says why;
        // the handler hears it's done.
        let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("coretext-DejaVuSans.ttf");
        std::fs::write(&path, DEJAVU).expect("a font file");
        let url = objc2_core_foundation::CFURL::from_file_path(&path).expect("a URL");
        let urls = objc2_core_foundation::CFArray::<objc2_core_foundation::CFURL>::from_objects(&[&*url]);
        let urls: &objc2_core_foundation::CFArray = &*(&*urls as *const objc2_core_foundation::CFArray<_>).cast();
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = finished.clone();
        let handler = block2::RcBlock::new(move |_errors: NonNull<objc2_core_foundation::CFArray>, done: u8| -> u8 {
            if done != 0 {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            1
        });
        let handler: &block2::DynBlock<dyn Fn(NonNull<objc2_core_foundation::CFArray>, u8) -> u8> = &handler;
        // SAFETY: bytes and bools have the same layout.
        let handler: &block2::DynBlock<dyn Fn(NonNull<objc2_core_foundation::CFArray>, bool) -> bool> =
            std::mem::transmute(handler);
        CTFontManagerRegisterFontURLs(urls, CTFontManagerScope::Process, true, Some(handler));
        // The handler may run on another thread: wait for it, a while.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while finished.load(std::sync::atomic::Ordering::SeqCst) == 0 && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(finished.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!CTFontManagerRegisterFontsForURL(&url, CTFontManagerScope::Process, &mut error));
        assert_eq!(manager_error(error), 105);
        assert!(CTFontManagerUnregisterFontsForURL(&url, CTFontManagerScope::Process, std::ptr::null_mut()));
        assert!(!CTFontManagerUnregisterFontsForURL(&url, CTFontManagerScope::Process, &mut error));
        assert_eq!(manager_error(error), 201);
        // The feature registry's numbers used above.
        assert_eq!((kLigaturesType, kCommonLigaturesOffSelector, kRareLigaturesOffSelector), (1, 3, 5));
        assert_eq!((kContextualAlternatesType, kContextualAlternatesOffSelector), (36, 1));
        assert_eq!(CTFontOrientation::Horizontal.0, 1);
    }
}

/// A font manager error's code, released; it's in the manager's domain.
fn manager_error(error: *mut objc2_core_foundation::CFError) -> CFIndex {
    assert!(!error.is_null(), "an error");
    // SAFETY: the error is the caller's (+1).
    let error = unsafe { CFRetained::from_raw(NonNull::new(error).unwrap()) };
    let domain = error.domain().expect("a domain").to_string();
    assert_eq!(domain, unsafe { kCTFontManagerErrorDomain }.to_string());
    error.code()
}

/// A font file without a character map still makes a font: glyphs by id
/// measure and draw (macOS lays text out in it too, by glyph names; text
/// in it falls back on other fonts on Linux).
#[test]
fn font_without_a_character_map() {
    let mut bytes = DEJAVU.to_vec();
    let tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    for i in 0..tables {
        let at = 12 + 16 * i;
        if &bytes[at..at + 4] == b"cmap" {
            // Renamed in place, keeping the directory in order.
            bytes[at..at + 4].copy_from_slice(b"cmaq");
        }
    }
    let data = CFData::from_bytes(&bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data)).expect("a provider");
    let file = CGFont::with_data_provider(&provider).expect("a font");
    // SAFETY: a font file, glyphs and positions for two.
    unsafe {
        let font = CTFont::with_graphics_font(&file, 24.0, std::ptr::null(), None);
        assert_eq!(font.post_script_name().to_string(), "DejaVuSans");
        assert_eq!(font.ascent(), 22.27734375);
        assert_eq!(glyphs_of(&font, "AV"), (vec![0, 0], false));
        let glyphs = [36u16, 57];
        let g = NonNull::new(glyphs.as_ptr().cast_mut()).unwrap();
        let mut advances = [CGSize::ZERO; 2];
        let total = font.advances_for_glyphs(CTFontOrientation::Horizontal, g, advances.as_mut_ptr(), 2);
        assert_eq!(total, 32.8359375);
        let c = ctx(60, 40);
        let p = [CGPoint::new(4.0, 10.0), CGPoint::new(20.0, 10.0)];
        font.draw_glyphs(g, NonNull::new(p.as_ptr().cast_mut()).unwrap(), 2, &c);
        ink_near(&c, [4, 12, 37, 30]);
    }
}

/// Every constant, with the value macOS gives it.
#[test]
#[allow(deprecated)]
fn constants() {
    // SAFETY: the constants are immutable strings.
    let all: &[(&str, &CFString)] = unsafe {
        &[
            ("CTFontCopyrightName", kCTFontCopyrightNameKey),
            ("CTFontFamilyName", kCTFontFamilyNameKey),
            ("CTFontSubFamilyName", kCTFontSubFamilyNameKey),
            ("CTFontSubFamilyName", kCTFontStyleNameKey),
            ("CTFontUniqueName", kCTFontUniqueNameKey),
            ("CTFontFullName", kCTFontFullNameKey),
            ("CTFontVersionName", kCTFontVersionNameKey),
            ("CTFontPostScriptName", kCTFontPostScriptNameKey),
            ("CTFontTrademarkName", kCTFontTrademarkNameKey),
            ("CTFontManufacturerName", kCTFontManufacturerNameKey),
            ("CTFontDesignerName", kCTFontDesignerNameKey),
            ("CTFontDescriptionName", kCTFontDescriptionNameKey),
            ("CTFontVendorURLName", kCTFontVendorURLNameKey),
            ("CTFontDesignerURLName", kCTFontDesignerURLNameKey),
            ("CTFontLicenseNameName", kCTFontLicenseNameKey),
            ("CTFontLicenseURLName", kCTFontLicenseURLNameKey),
            ("CTFontSampleTextName", kCTFontSampleTextNameKey),
            ("CTFontPostScriptCIDName", kCTFontPostScriptCIDNameKey),
            ("NSCTVariationAxisIdentifier", kCTFontVariationAxisIdentifierKey),
            ("NSCTVariationAxisMinimumValue", kCTFontVariationAxisMinimumValueKey),
            ("NSCTVariationAxisMaximumValue", kCTFontVariationAxisMaximumValueKey),
            ("NSCTVariationAxisDefaultValue", kCTFontVariationAxisDefaultValueKey),
            ("NSCTVariationAxisName", kCTFontVariationAxisNameKey),
            ("NSCTVariationAxisHidden", kCTFontVariationAxisHiddenKey),
            ("CTFeatureOpenTypeTag", kCTFontOpenTypeFeatureTag),
            ("CTFeatureOpenTypeValue", kCTFontOpenTypeFeatureValue),
            ("CTFeatureTypeIdentifier", kCTFontFeatureTypeIdentifierKey),
            ("CTFeatureTypeName", kCTFontFeatureTypeNameKey),
            ("CTFeatureTypeExclusive", kCTFontFeatureTypeExclusiveKey),
            ("CTFeatureTypeSelectors", kCTFontFeatureTypeSelectorsKey),
            ("CTFeatureSelectorIdentifier", kCTFontFeatureSelectorIdentifierKey),
            ("CTFeatureSelectorName", kCTFontFeatureSelectorNameKey),
            ("CTFeatureSelectorDefault", kCTFontFeatureSelectorDefaultKey),
            ("CTFeatureSelectorSetting", kCTFontFeatureSelectorSettingKey),
            ("CTFeatureSampleText", kCTFontFeatureSampleTextKey),
            ("CTFeatureTooltipText", kCTFontFeatureTooltipTextKey),
            ("CTBaselineClassRoman", kCTBaselineClassRoman),
            ("CTBaselineClassIdeographicCentered", kCTBaselineClassIdeographicCentered),
            ("CTBaselineClassIdeographicLow", kCTBaselineClassIdeographicLow),
            ("CTBaselineClassIdeographicHigh", kCTBaselineClassIdeographicHigh),
            ("CTBaselineClassHanging", kCTBaselineClassHanging),
            ("CTBaselineClassMath", kCTBaselineClassMath),
            ("CTBaselineReferenceFont", kCTBaselineReferenceFont),
            ("CTBaselineOriginalFont", kCTBaselineOriginalFont),
            ("NSCTFontCollectionRemoveDuplicatesOption", kCTFontCollectionRemoveDuplicatesOption),
            ("NSCTFontCollectionIncludeDisabledFontsOption", kCTFontCollectionIncludeDisabledFontsOption),
            ("NSCTFontCollectionDisallowAutoActivationOption", kCTFontCollectionDisallowAutoActivationOption),
            ("NSCTFontFileURLAttribute", kCTFontURLAttribute),
            ("NSFontNameAttribute", kCTFontNameAttribute),
            ("NSFontVisibleNameAttribute", kCTFontDisplayNameAttribute),
            ("NSFontFamilyAttribute", kCTFontFamilyNameAttribute),
            ("NSFontFaceAttribute", kCTFontStyleNameAttribute),
            ("NSCTFontTraitsAttribute", kCTFontTraitsAttribute),
            ("NSCTFontVariationAttribute", kCTFontVariationAttribute),
            ("NSCTFontVariationAxesAttribute", kCTFontVariationAxesAttribute),
            ("NSFontSizeAttribute", kCTFontSizeAttribute),
            ("NSCTFontMatrixAttribute", kCTFontMatrixAttribute),
            ("NSCTFontCascadeListAttribute", kCTFontCascadeListAttribute),
            ("NSCTFontCharacterSetAttribute", kCTFontCharacterSetAttribute),
            ("NSCTFontLanguagesAttribute", kCTFontLanguagesAttribute),
            ("NSCTFontBaselineAdjustAttribute", kCTFontBaselineAdjustAttribute),
            ("NSCTFontMacintoshEncodingsAttribute", kCTFontMacintoshEncodingsAttribute),
            ("NSCTFontFeaturesAttribute", kCTFontFeaturesAttribute),
            ("NSCTFontFeatureSettingsAttribute", kCTFontFeatureSettingsAttribute),
            ("NSCTFontFixedAdvanceAttribute", kCTFontFixedAdvanceAttribute),
            ("NSCTFontOrientationAttribute", kCTFontOrientationAttribute),
            ("NSCTFontFormatAttribute", kCTFontFormatAttribute),
            ("NSCTFontRegistrationScopeAttribute", kCTFontRegistrationScopeAttribute),
            ("NSCTFontPriorityAttribute", kCTFontPriorityAttribute),
            ("NSCTFontEnabledAttribute", kCTFontEnabledAttribute),
            ("NSCTFontDownloadableAttribute", kCTFontDownloadableAttribute),
            ("NSCTFontDownloadedAttribute", kCTFontDownloadedAttribute),
            ("NSCTFontOpticalSizeAttribute", kCTFontOpticalSizeAttribute),
            ("CTFontDescriptorMatchingSourceDescriptor", kCTFontDescriptorMatchingSourceDescriptor),
            ("CTFontDescriptorMatchingDescriptors", kCTFontDescriptorMatchingDescriptors),
            ("CTFontDescriptorMatchingResult", kCTFontDescriptorMatchingResult),
            ("CTFontDescriptorMatchingPercentage", kCTFontDescriptorMatchingPercentage),
            ("CTFontDescriptorMatchingCurrentAssetSize", kCTFontDescriptorMatchingCurrentAssetSize),
            ("CTFontDescriptorMatchingTotalDownloadedSize", kCTFontDescriptorMatchingTotalDownloadedSize),
            ("CTFontDescriptorMatchingTotalAssetSize", kCTFontDescriptorMatchingTotalAssetSize),
            ("CTFontDescriptorMatchingError", kCTFontDescriptorMatchingError),
            ("CTFontRegistrationUserInfoAttribute", kCTFontRegistrationUserInfoAttribute),
            ("com.apple.CoreText", kCTFontManagerBundleIdentifier),
            ("CTFontManagerFontChangedNotification", kCTFontManagerRegisteredFontsChangedNotification),
            ("com.apple.CoreText.CTFontManagerErrorDomain", kCTFontManagerErrorDomain),
            ("CTFontManagerErrorFontURLs", kCTFontManagerErrorFontURLsKey),
            ("CTFontManagerErrorFontDescriptors", kCTFontManagerErrorFontDescriptorsKey),
            ("CTFontManagerErrorFontAssetNameKey", kCTFontManagerErrorFontAssetNameKey),
            ("NSCTFontSymbolicTrait", kCTFontSymbolicTrait),
            ("NSCTFontWeightTrait", kCTFontWeightTrait),
            ("NSCTFontProportionTrait", kCTFontWidthTrait),
            ("NSCTFontSlantTrait", kCTFontSlantTrait),
            ("CTFrameProgression", kCTFrameProgressionAttributeName),
            ("CTFramePathFillRule", kCTFramePathFillRuleAttributeName),
            ("CTFramePathWidth", kCTFramePathWidthAttributeName),
            ("CTFrameClippingPaths", kCTFrameClippingPathsAttributeName),
            ("CTFramePathClippingPath", kCTFramePathClippingPathAttributeName),
            ("CTRubyAnnotationSizeFactor", kCTRubyAnnotationSizeFactorAttributeName),
            ("CTRubyAnnotationScaleToFit", kCTRubyAnnotationScaleToFitAttributeName),
            ("NSFont", kCTFontAttributeName),
            ("CTForegroundColorFromContext", kCTForegroundColorFromContextAttributeName),
            ("NSKern", kCTKernAttributeName),
            ("CTTracking", kCTTrackingAttributeName),
            ("NSLigature", kCTLigatureAttributeName),
            ("CTForegroundColor", kCTForegroundColorAttributeName),
            ("CTBackgroundColor", kCTBackgroundColorAttributeName),
            ("NSParagraphStyle", kCTParagraphStyleAttributeName),
            ("NSStrokeWidth", kCTStrokeWidthAttributeName),
            ("CTStrokeColor", kCTStrokeColorAttributeName),
            ("NSUnderline", kCTUnderlineStyleAttributeName),
            ("CTSuperscript", kCTSuperscriptAttributeName),
            ("CTUnderlineColor", kCTUnderlineColorAttributeName),
            ("CTVerticalForms", kCTVerticalFormsAttributeName),
            ("CTHorizontalInVerticalForms", kCTHorizontalInVerticalFormsAttributeName),
            ("NSGlyphInfo", kCTGlyphInfoAttributeName),
            ("NSCharacterShape", kCTCharacterShapeAttributeName),
            ("NSLanguage", kCTLanguageAttributeName),
            ("CTRunDelegate", kCTRunDelegateAttributeName),
            ("CTBaselineClass", kCTBaselineClassAttributeName),
            ("CTBaselineInfo", kCTBaselineInfoAttributeName),
            ("CTBaselineReferenceInfo", kCTBaselineReferenceInfoAttributeName),
            ("CTBaselineOffset", kCTBaselineOffsetAttributeName),
            ("NSWritingDirection", kCTWritingDirectionAttributeName),
            ("CTRubyAnnotation", kCTRubyAnnotationAttributeName),
            ("CTAdaptiveImageProvider", kCTAdaptiveImageProviderAttributeName),
            ("NSTabColumnTerminatorsAttributeName", kCTTabColumnTerminatorsAttributeName),
            ("CTTypesetterOptionAllowUnboundedLayout", kCTTypesetterOptionAllowUnboundedLayout),
            ("CTTypesetterOptionDisableBidiProcessing", kCTTypesetterOptionDisableBidiProcessing),
            ("CTTypesetterOptionForcedEmbeddingLevel", kCTTypesetterOptionForcedEmbeddingLevel),
        ]
    };
    for (value, constant) in all {
        assert_eq!(constant.to_string(), *value);
    }
}
