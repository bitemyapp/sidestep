//! What CoreText records and registers, which conformance tests can't see:
//! glyph runs for upright text, outlines otherwise; font files registered
//! once, under a private family that lists of families leave out; and
//! a CTFont made from a file being an NSFont reporting the file's names.

use std::sync::Arc;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFAttributedString, CFString, CGAffineTransform};
use objc2_core_graphics::{CGContext, CGFont};
use objc2_core_text::{CTFont, CTLine, kCTFontAttributeName};
use objc2_foundation::{NSAttributedString, NSDictionary, NSString};
use parley::FontData;
use parley::fontique::Blob;

use super::{font, line};
use crate::graphics::{self, Xf};
use crate::protocol::{Op, Rect};
use crate::text::fonts;

const DEJAVU: &[u8] = include_bytes!("../../../../conformance/tests/fixtures/DejaVuSans.ttf");

fn file_font(size: f64) -> Retained<CTFont> {
    let blob = Blob::new(Arc::new(DEJAVU.to_vec()));
    let cg = crate::coregraphics::font::from_data(FontData::new(blob, 0)).expect("a font");
    // SAFETY: a CGFontImpl is what CGFont names.
    let cg: &CGFont = unsafe { &*Retained::as_ptr(&cg).cast() };
    let made = font::CTFontCreateWithGraphicsFont(Some(cg), size, std::ptr::null(), None).expect("a CTFont");
    // SAFETY: a +1 font.
    unsafe { Retained::from_raw(made.as_ptr()) }.expect("a font")
}

fn line_of(text: &str, font: &CTFont) -> Retained<CTLine> {
    // SAFETY: the constant is a string, the font an object.
    let key: &NSString = unsafe { &*(kCTFontAttributeName as *const CFString).cast() };
    let value: &AnyObject = unsafe { &*(font as *const CTFont).cast() };
    let dict = NSDictionary::from_slices(&[key], &[value]);
    // SAFETY: a string and an attribute dictionary.
    let string = unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(text),
            Some(&dict),
        )
    };
    // SAFETY: an NSAttributedString is a CFAttributedString.
    let cf: &CFAttributedString = unsafe { &*Retained::as_ptr(&string).cast() };
    let made = line::CTLineCreateWithAttributedString(Some(cf)).expect("a line");
    // SAFETY: a +1 line.
    unsafe { Retained::from_raw(made.as_ptr()) }.expect("a line")
}

/// The ops `draw` records with the current context's CGContext, in a view
/// of 200 × 100 points that is flipped (or not).
fn record(flipped: bool, draw: impl FnOnce(&CGContext)) -> Vec<Op> {
    graphics::begin_recording();
    let xf = if flipped { Xf::IDENTITY } else { Xf { tx: 0.0, a: -1.0, ty: 100.0 } };
    graphics::set_view(xf, Rect::new(0.0, 0.0, 200.0, 100.0));
    let context = crate::context::current().expect("a context");
    let cg = context.CGContext();
    draw(&cg);
    graphics::end_recording()
}

fn set_text_matrix(cg: &CGContext, d: f64) {
    let m = CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d, tx: 0.0, ty: 0.0 };
    crate::coregraphics::context::CGContextSetTextMatrix(Some(cg), m);
}

#[test]
fn upright_lines_record_glyph_runs() {
    let font = file_font(24.0);
    let line = line_of("Hx", &font);
    for flipped in [false, true] {
        let ops = record(flipped, |cg| {
            // A flipped view turns text back over with the text matrix.
            set_text_matrix(cg, if flipped { -1.0 } else { 1.0 });
            crate::coregraphics::context::CGContextSetTextPosition(Some(cg), 10.0, 40.0);
            line::CTLineDraw(Some(&line), Some(cg));
        });
        let runs: Vec<_> = ops.iter().filter_map(|op| if let Op::Glyphs(r) = op { Some(r) } else { None }).collect();
        assert_eq!(runs.len(), 1, "{ops:?}");
        let run = runs[0];
        assert_eq!(run.size, 24.0);
        assert_eq!(run.glyphs.iter().map(|g| g.id).collect::<Vec<_>>(), [43, 91]);
        // The baseline 40 points from the view's origin: its bottom, or
        // (flipped) its top.
        assert_eq!((run.x, run.y), (10.0, if flipped { 40.0 } else { 60.0 }));
        assert!((run.glyphs[1].x - 18.046875).abs() < 1e-3);
        let face = fonts::face_data(run.font).expect("a registered face");
        assert_eq!(face.font.data.data(), DEJAVU);
    }
}

#[test]
fn turned_mirrored_or_shadowed_text_draws_outlines() {
    let font = file_font(24.0);
    let line = line_of("Hx", &font);
    let turned = record(false, |cg| {
        crate::coregraphics::context::CGContextRotateCTM(Some(cg), 0.5);
        line::CTLineDraw(Some(&line), Some(cg));
    });
    // A flipped view with nothing turning the text back: upside down.
    let mirrored = record(true, |cg| line::CTLineDraw(Some(&line), Some(cg)));
    let shadowed = record(false, |cg| {
        let offset = objc2_core_foundation::CGSize { width: 2.0, height: -2.0 };
        crate::coregraphics::context::CGContextSetShadow(Some(cg), offset, 1.0);
        line::CTLineDraw(Some(&line), Some(cg));
    });
    for (name, ops) in [("turned", &turned), ("mirrored", &mirrored), ("shadowed", &shadowed)] {
        assert!(ops.iter().all(|op| !matches!(op, Op::Glyphs(_))), "{name}: {ops:?}");
        assert!(ops.iter().any(|op| matches!(op, Op::FillPath { .. })), "{name}: {ops:?}");
    }
    let Some(Op::FillPath { draw, .. }) = shadowed.iter().find(|op| matches!(op, Op::FillPath { .. })) else {
        unreachable!()
    };
    assert!(draw.shadow.is_some());
}

#[test]
fn glyphs_by_position_go_through_the_text_matrix() {
    let font = file_font(20.0);
    let glyphs = [43u16];
    let positions = [objc2_core_foundation::CGPoint { x: 5.0, y: 10.0 }];
    let ops = record(false, |cg| {
        set_text_matrix(cg, 1.0);
        crate::coregraphics::context::CGContextSetTextPosition(Some(cg), 30.0, 0.0);
        // SAFETY: one glyph and one position.
        unsafe { font::CTFontDrawGlyphs(Some(&font), glyphs.as_ptr(), positions.as_ptr(), 1, Some(cg)) };
    });
    let Some(Op::Glyphs(run)) = ops.first() else { panic!("glyphs: {ops:?}") };
    assert_eq!((run.x, run.y, run.size), (35.0, 90.0, 20.0));
}

#[test]
fn font_files_register_once_under_a_private_family() {
    let a = FontData::new(Blob::new(Arc::new(DEJAVU.to_vec())), 0);
    let b = FontData::new(Blob::new(Arc::new(DEJAVU.to_vec())), 0);
    let (fa, fb) = (fonts::register_data(&a).unwrap(), fonts::register_data(&b).unwrap());
    assert_eq!(fa, fb, "the same file registers once");
    assert!(fa.0.family.starts_with(fonts::PRIVATE_FAMILY));
    assert!(fonts::register_data(&FontData::new(Blob::new(Arc::new(vec![0u8; 64])), 0)).is_none());
    // Its fonts report the file's own names; lists of families leave the
    // private one out.
    let font = file_font(12.0);
    let ns = super::ns_font(&font);
    assert_eq!(ns.familyName().unwrap().to_string(), "DejaVu Sans");
    assert_eq!(ns.fontName().to_string(), "DejaVuSans");
    let names = super::manager::CTFontManagerCopyAvailableFontFamilyNames().unwrap();
    // SAFETY: a +1 array of strings.
    let names: Retained<objc2_foundation::NSArray<NSString>> =
        unsafe { Retained::from_raw(names.as_ptr().cast()) }.unwrap();
    assert!(names.iter().all(|n| !n.to_string().starts_with(fonts::PRIVATE_FAMILY)));
    // A copy of the font at its own size is the font itself.
    let same = font::CTFontCreateCopyWithAttributes(Some(&font), 0.0, std::ptr::null(), None).unwrap();
    assert!(std::ptr::eq(same.as_ptr().cast_const(), &*font as *const CTFont));
    // SAFETY: the +1 copy.
    drop(unsafe { Retained::from_raw(same.as_ptr()) });
}

#[test]
fn feature_registry_maps_onto_opentype_features() {
    use crate::font::{registry_feature, registry_setting};
    use objc2_core_text::{
        kCommonLigaturesOffSelector, kCommonLigaturesOnSelector, kContextualAlternatesOffSelector,
        kContextualAlternatesType, kLigaturesType, kRareLigaturesOffSelector, kUpperCaseSmallCapsSelector,
        kUpperCaseType,
    };
    let pairs = [
        ((kLigaturesType, kCommonLigaturesOffSelector as i32), (*b"liga", 0)),
        ((kLigaturesType, kCommonLigaturesOnSelector as i32), (*b"liga", 1)),
        ((kLigaturesType, kRareLigaturesOffSelector as i32), (*b"dlig", 0)),
        ((kContextualAlternatesType, kContextualAlternatesOffSelector as i32), (*b"calt", 0)),
        ((kUpperCaseType, kUpperCaseSmallCapsSelector as i32), (*b"c2sc", 1)),
    ];
    for ((kind, selector), feature) in pairs {
        assert_eq!(registry_feature(i64::from(kind), i64::from(selector)), Some(feature));
        assert_eq!(registry_setting(feature.0, feature.1), Some((i64::from(kind), i64::from(selector))));
    }
    assert_eq!(registry_feature(22, 8), Some((*b"kern", 0)));
    assert_eq!(registry_feature(1, 99), None);
}

#[test]
fn looking_at_a_file_remembers_nothing() {
    // DejaVu with a byte of its `FFTM` timestamp changed: a file no test
    // loads otherwise.
    let mut bytes = DEJAVU.to_vec();
    let at = (0..u16::from_be_bytes([bytes[4], bytes[5]]) as usize)
        .map(|i| 12 + 16 * i)
        .find(|&at| &bytes[at..at + 4] == b"FFTM")
        .expect("an FFTM table");
    let offset = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
    bytes[offset + 11] ^= 0x55;
    let font = FontData::new(Blob::new(Arc::new(bytes)), 0);
    let looked = fonts::peek_data(&font).expect("a face");
    assert!(fonts::data_face(&font).is_none(), "a look registers nothing");
    let registered = fonts::register_data(&font).expect("a face");
    assert_ne!(looked, registered);
    assert_eq!(fonts::data_face(&font), Some(registered));
}

#[test]
fn glyph_boxes_are_remembered() {
    let font = file_font(24.0);
    let face = font::parts(&font).1.clone();
    let first = font::glyph_bounds(&face, [36u16, 3, 60000].into_iter());
    assert!(first[0].is_some() && first[1].is_none() && first[2].is_none());
    let mut known = 0;
    face.boxes.lookup(&[36, 3, 60000], |_, found| known += usize::from(found.is_some()));
    assert_eq!(known, 3);
    assert_eq!(font::glyph_bounds(&face, [36u16, 3, 60000].into_iter()), first);
}

#[test]
fn collections_sort_with_whatever_the_callback_says() {
    use objc2_core_foundation::{CFComparisonResult, CFIndex};
    use objc2_core_text::CTFontDescriptor;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    // An order that isn't one, and a check that the program's null pointer
    // comes through as null.
    unsafe extern "C-unwind" fn contrary(
        _a: NonNull<CTFontDescriptor>,
        _b: NonNull<CTFontDescriptor>,
        ref_con: *mut c_void,
    ) -> CFComparisonResult {
        assert!(ref_con.is_null());
        let n = CALLS.fetch_add(1, Ordering::Relaxed);
        CFComparisonResult([-1, 1, 0, 1, -1][n % 5] as CFIndex)
    }
    type Declared = unsafe extern "C-unwind" fn(
        NonNull<CTFontDescriptor>,
        NonNull<CTFontDescriptor>,
        NonNull<c_void>,
    ) -> CFComparisonResult;
    // SAFETY: the types differ only in the pointer's non-null promise, which
    // C callers don't keep.
    let contrary: Declared = unsafe { std::mem::transmute(contrary as unsafe extern "C-unwind" fn(_, _, _) -> _) };
    let all = super::collection::CTFontCollectionCreateFromAvailableFonts(None).expect("a collection");
    // SAFETY: a +1 collection.
    let all: Retained<objc2_core_text::CTFontCollection> = unsafe { Retained::from_raw(all.as_ptr()) }.unwrap();
    let sorted = super::collection::CTFontCollectionCreateMatchingFontDescriptorsSortedWithCallback(
        Some(&all),
        Some(contrary),
        std::ptr::null_mut(),
    );
    if let Some(sorted) = sorted {
        // SAFETY: the +1 array.
        drop(unsafe { Retained::from_raw(sorted.as_ptr()) });
    }
}
