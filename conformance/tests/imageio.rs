//! ImageIO, checked against macOS: image sources over files of each type
//! the codecs read (their type, images, properties and thumbnails, as
//! ImageIO describes them), sources whose data arrives a piece at a time,
//! and destinations writing files that sources read back. The files are
//! `tests/fixtures` (made by `scripts/make-image-fixtures`, never with
//! Apple's tools). ImageIO runs on any thread, so the default harness does.
//!
//! Where ImageIO reads more than the codecs (HEIC, RAW and the rest; more
//! pages of a TIFF), the tests stay with what both read; see
//! docs/architecture.md, "ImageIO".

use objc2_core_foundation::{
    CFArray, CFBoolean, CFData, CFDictionary, CFGetTypeID, CFMutableData, CFNumber, CFNumberType, CFRetained, CFString,
    CFType, CFURL, CGPoint, CGRect, CGSize, ConcreteType, kCFBooleanFalse, kCFBooleanTrue,
};
use objc2_core_graphics::*;
use objc2_image_io::*;

use sidestep as _;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).expect("the fixture")
}

fn source(name: &str) -> CFRetained<CGImageSource> {
    source_of(&fixture(name))
}

fn source_of(bytes: &[u8]) -> CFRetained<CGImageSource> {
    // SAFETY: plain data, no options.
    unsafe { CGImageSource::with_data(&CFData::from_bytes(bytes), None) }.expect("a source")
}

fn cf<T: ?Sized>(v: &T) -> &CFType {
    // SAFETY: every CoreFoundation object is a CFType.
    unsafe { &*(v as *const T).cast::<CFType>() }
}

/// A dictionary of `entries`, CoreFoundation values under ImageIO's keys.
fn dict(entries: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary> {
    let keys: Vec<&CFString> = entries.iter().map(|e| e.0).collect();
    let values: Vec<&CFType> = entries.iter().map(|e| e.1).collect();
    let d = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    // SAFETY: the same dictionary, as the untyped type.
    unsafe { CFRetained::cast_unchecked(d) }
}

fn yes() -> &'static CFType {
    cf(unsafe { kCFBooleanTrue }.expect("true"))
}

fn no() -> &'static CFType {
    cf(unsafe { kCFBooleanFalse }.expect("false"))
}

fn value(d: &CFDictionary, key: &str) -> Option<CFRetained<CFType>> {
    // SAFETY: ImageIO's dictionaries have string keys.
    let d = unsafe { &*(d as *const CFDictionary).cast::<CFDictionary<CFString, CFType>>() };
    d.get(&CFString::from_str(key))
}

#[track_caller]
fn number(d: &CFDictionary, key: &str) -> f64 {
    let v = value(d, key).unwrap_or_else(|| panic!("{key} in {d:?}"));
    v.downcast_ref::<CFNumber>().and_then(|n| n.as_f64()).unwrap_or_else(|| panic!("{key} is a number"))
}

/// A number's type and value: ImageIO gives sizes as `long long`s,
/// orientations as `int`s, densities as `float`s, delays as `double`s.
#[track_caller]
fn typed(d: &CFDictionary, key: &str) -> (CFNumberType, f64) {
    let v = value(d, key).unwrap_or_else(|| panic!("{key} in {d:?}"));
    let n = v.downcast_ref::<CFNumber>().unwrap_or_else(|| panic!("{key} is a number"));
    (n.r#type(), n.as_f64().expect("a value"))
}

#[track_caller]
fn string(d: &CFDictionary, key: &str) -> String {
    let v = value(d, key).unwrap_or_else(|| panic!("{key} in {d:?}"));
    v.downcast_ref::<CFString>().unwrap_or_else(|| panic!("{key} is a string")).to_string()
}

#[track_caller]
fn flag(d: &CFDictionary, key: &str) -> Option<bool> {
    value(d, key).map(|v| v.downcast_ref::<CFBoolean>().unwrap_or_else(|| panic!("{key} is a boolean")).as_bool())
}

#[track_caller]
fn sub(d: &CFDictionary, key: &str) -> CFRetained<CFDictionary> {
    let v = value(d, key).unwrap_or_else(|| panic!("{key} in {d:?}"));
    v.downcast::<CFDictionary>().unwrap_or_else(|_| panic!("{key} is a dictionary"))
}

#[track_caller]
fn numbers(d: &CFDictionary, key: &str) -> Vec<f64> {
    let v = value(d, key).unwrap_or_else(|| panic!("{key} in {d:?}"));
    let a = v.downcast::<CFArray>().unwrap_or_else(|_| panic!("{key} is an array"));
    // SAFETY: an array of numbers.
    let a = unsafe { &*(&*a as *const CFArray).cast::<CFArray<CFNumber>>() };
    a.iter().map(|n| n.as_f64().expect("a number")).collect()
}

fn props(s: &CGImageSource, i: usize) -> CFRetained<CFDictionary> {
    // SAFETY: no options.
    unsafe { s.properties_at_index(i, None) }.unwrap_or_else(|| panic!("properties at {i}"))
}

fn file_props(s: &CGImageSource) -> CFRetained<CFDictionary> {
    // SAFETY: no options.
    unsafe { s.properties(None) }.expect("the file's properties")
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// `image` drawn into an RGBA context of its size: pixel (`x`, `y`) from
/// the top left.
fn pixel(image: &CGImage, x: usize, y: usize) -> [u8; 4] {
    let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).expect("sRGB");
    // SAFETY: no data: the context allocates.
    let ctx = unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, Some(&space), CGImageAlphaInfo::PremultipliedLast.0)
    }
    .expect("a context");
    CGContext::draw_image(
        Some(&ctx),
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64)),
        Some(image),
    );
    let data = CGBitmapContextGetData(Some(&ctx)) as *const u8;
    let bpr = CGBitmapContextGetBytesPerRow(Some(&ctx));
    // SAFETY: inside the context's memory.
    unsafe { std::ptr::read(data.add(y * bpr + x * 4).cast::<[u8; 4]>()) }
}

/// Whether each channel of `a` is within `tol` of `b`'s.
fn near(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= tol)
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

fn type_of(s: &CGImageSource) -> Option<String> {
    // SAFETY: a source.
    unsafe { s.r#type() }.map(|t| t.to_string())
}

#[test]
fn type_ids_and_identifiers() {
    let s = source("rgba-72dpi.png");
    assert_eq!(CFGetTypeID(Some(cf(&*s))), CGImageSource::type_id());
    assert_ne!(CGImageSource::type_id(), CGImageDestination::type_id());
    assert_ne!(CGImageSource::type_id(), CGImage::type_id());
    let listed = |a: CFRetained<CFArray>| -> Vec<String> {
        // SAFETY: an array of strings.
        let a = unsafe { &*(&*a as *const CFArray).cast::<CFArray<CFString>>() };
        a.iter().map(|s| s.to_string()).collect()
    };
    // SAFETY: plain calls.
    let (read, written) = unsafe { (CGImageSource::type_identifiers(), CGImageDestination::type_identifiers()) };
    let (read, written) = (listed(read), listed(written));
    for t in [
        "public.png",
        "public.jpeg",
        "com.compuserve.gif",
        "org.webmproject.webp",
        "com.microsoft.bmp",
        "public.tiff",
        "com.microsoft.ico",
    ] {
        assert!(read.iter().any(|r| r == t), "sources read {t}: {read:?}");
    }
    for t in ["public.png", "public.jpeg", "com.compuserve.gif", "public.tiff", "com.microsoft.bmp"] {
        assert!(written.iter().any(|r| r == t), "destinations write {t}: {written:?}");
    }
    // In ImageIO's order: JPEG, then PNG, then GIF.
    assert_eq!(read[..3], ["public.jpeg", "public.png", "com.compuserve.gif"]);
    assert_eq!(written[..3], ["public.jpeg", "public.png", "com.compuserve.gif"]);
}

#[test]
fn png_properties() {
    let s = source("rgba-144dpi.png");
    // SAFETY: a source.
    unsafe {
        assert_eq!(type_of(&s).as_deref(), Some("public.png"));
        assert_eq!((s.count(), s.status(), s.primary_image_index()), (1, CGImageSourceStatus::StatusComplete, 0));
    }
    let f = file_props(&s);
    assert_eq!(typed(&f, "FileSize"), (CFNumberType::SInt64Type, 123.0));
    let p = props(&s, 0);
    assert_eq!(typed(&p, "PixelWidth"), (CFNumberType::SInt64Type, 4.0));
    assert_eq!(typed(&p, "PixelHeight"), (CFNumberType::SInt64Type, 2.0));
    assert_eq!(typed(&p, "Depth"), (CFNumberType::SInt64Type, 8.0));
    assert_eq!(typed(&p, "DPIWidth"), (CFNumberType::Float32Type, 144.0));
    assert_eq!(typed(&p, "DPIHeight"), (CFNumberType::Float32Type, 144.0));
    assert_eq!(string(&p, "ColorModel"), "RGB");
    assert_eq!(flag(&p, "HasAlpha"), Some(true));
    assert!(value(&p, "ProfileName").is_none());
    assert!(value(&p, "Orientation").is_none());
    let png = sub(&p, "{PNG}");
    assert_eq!(typed(&png, "XPixelsPerMeter"), (CFNumberType::SInt32Type, 5669.0));
    assert_eq!(number(&png, "InterlaceType"), 0.0);
    // No density, no density keys.
    let p = props(&source("halves.png"), 0);
    assert!(value(&p, "DPIWidth").is_none() && value(&sub(&p, "{PNG}"), "XPixelsPerMeter").is_none());
    // Gray, and 16 bits a sample.
    let p = props(&source("gray.png"), 0);
    assert_eq!((string(&p, "ColorModel"), number(&p, "Depth"), flag(&p, "HasAlpha")), ("Gray".into(), 8.0, None));
    let p = props(&source("rgb16.png"), 0);
    assert_eq!((string(&p, "ColorModel"), number(&p, "Depth")), ("RGB".into(), 16.0));
    // Gamma, the sRGB chunk (sRGB's name and chromaticities) and text.
    let p = props(&source("text.png"), 0);
    assert_eq!(string(&p, "ProfileName"), "sRGB IEC61966-2.1");
    let png = sub(&p, "{PNG}");
    assert!(close(number(&png, "Gamma"), 0.45455));
    assert_eq!(number(&png, "sRGBIntent"), 0.0);
    assert_eq!(string(&png, "Title"), "A title");
    assert_eq!(string(&png, "Author"), "Someone");
    assert_eq!(string(&png, "Description"), "Café");
    let c = numbers(&png, "Chromaticities");
    let want = [0.3127, 0.329, 0.64, 0.33, 0.3, 0.6, 0.15, 0.06];
    assert!(c.len() == 8 && c.iter().zip(want).all(|(a, b)| close(*a, b)), "{c:?}");
}

#[test]
fn jpeg_properties() {
    let s = source("exif-thumb.jpg");
    assert_eq!(type_of(&s).as_deref(), Some("public.jpeg"));
    let p = props(&s, 0);
    assert_eq!((number(&p, "PixelWidth"), number(&p, "PixelHeight")), (300.0, 200.0));
    assert_eq!(typed(&p, "Orientation"), (CFNumberType::SInt32Type, 6.0));
    assert_eq!(typed(&p, "DPIWidth"), (CFNumberType::Float32Type, 300.0));
    assert_eq!(string(&p, "ProfileName"), "sRGB IEC61966-2.1");
    assert_eq!(flag(&p, "HasAlpha"), None);
    let tiff = sub(&p, "{TIFF}");
    assert_eq!(typed(&tiff, "Orientation"), (CFNumberType::SInt32Type, 6.0));
    assert_eq!(string(&tiff, "Make"), "Sidestep");
    assert_eq!(string(&tiff, "Model"), "Fixture");
    assert_eq!(string(&tiff, "ImageDescription"), "A test");
    assert_eq!(string(&tiff, "Software"), "make-image-fixtures");
    assert_eq!(typed(&tiff, "XResolution"), (CFNumberType::Float64Type, 300.0));
    assert_eq!(typed(&tiff, "ResolutionUnit"), (CFNumberType::SInt32Type, 2.0));
    let exif = sub(&p, "{Exif}");
    assert_eq!(typed(&exif, "ExposureTime"), (CFNumberType::Float64Type, 0.01));
    assert!(close(number(&exif, "FNumber"), 2.8));
    assert_eq!(numbers(&exif, "ISOSpeedRatings"), [200.0]);
    assert_eq!(numbers(&exif, "ExifVersion"), [2.0, 3.0, 2.0]);
    assert_eq!(string(&exif, "DateTimeOriginal"), "2026:09:27 10:11:12");
    assert_eq!(number(&exif, "FocalLength"), 50.0);
    assert_eq!(typed(&exif, "PixelXDimension"), (CFNumberType::SInt32Type, 300.0));
    assert_eq!(number(&exif, "ColorSpace"), 1.0);
    let jfif = sub(&p, "{JFIF}");
    assert_eq!(numbers(&jfif, "JFIFVersion"), [1.0, 0.0, 2.0]);
    assert_eq!(typed(&jfif, "DensityUnit"), (CFNumberType::SInt32Type, 0.0));
    assert_eq!(number(&jfif, "XDensity"), 1.0);
    assert_eq!(flag(&jfif, "IsProgressive"), None);
    // The EXIF orientation alone.
    let p = props(&source("orientation-6.jpg"), 0);
    assert_eq!(number(&p, "Orientation"), 6.0);
    assert_eq!(number(&sub(&p, "{TIFF}"), "Orientation"), 6.0);
    assert!(value(&p, "{Exif}").is_none() && value(&p, "DPIWidth").is_none());
    let p = props(&source("plain.jpg"), 0);
    assert!(value(&p, "Orientation").is_none() && value(&p, "{TIFF}").is_none());
}

#[test]
fn gif_and_webp_frames() {
    let s = source("frames.gif");
    // SAFETY: a source.
    unsafe { assert_eq!((type_of(&s).as_deref(), s.count()), (Some("com.compuserve.gif"), 3)) };
    let gif = sub(&file_props(&s), "{GIF}");
    assert_eq!(typed(&gif, "LoopCount"), (CFNumberType::SInt32Type, 0.0));
    assert_eq!(typed(&gif, "CanvasPixelWidth"), (CFNumberType::SInt32Type, 4.0));
    assert_eq!(flag(&gif, "HasGlobalColorMap"), Some(true));
    for (i, delay) in [0.1, 0.2, 0.3].into_iter().enumerate() {
        let p = props(&s, i);
        assert_eq!((number(&p, "PixelWidth"), string(&p, "ProfileName")), (4.0, "sRGB IEC61966-2.1".into()));
        let g = sub(&p, "{GIF}");
        assert_eq!(typed(&g, "DelayTime"), (CFNumberType::Float64Type, delay));
        assert!(close(number(&g, "UnclampedDelayTime"), delay));
    }
    // Played once without a loop count; twice when the file says once more.
    assert_eq!(number(&sub(&file_props(&source("once.gif")), "{GIF}"), "LoopCount"), 1.0);
    assert_eq!(number(&sub(&file_props(&source("twice.gif")), "{GIF}"), "LoopCount"), 2.0);
    // One frame with no delay: shown for a tenth, unclamped nothing.
    let s = source("small.gif");
    let g = sub(&props(&s, 0), "{GIF}");
    assert_eq!((number(&g, "DelayTime"), number(&g, "UnclampedDelayTime")), (0.1, 0.0));
    assert_eq!(number(&sub(&file_props(&s), "{GIF}"), "LoopCount"), 1.0);
    assert_eq!(flag(&props(&s, 0), "HasAlpha"), None);
    // Each frame is an image of its own, as it shows.
    for (i, color) in [RED, GREEN, BLUE].into_iter().enumerate() {
        // SAFETY: a source.
        let image = unsafe { source("frames.gif").image_at_index(i, None) }.expect("a frame");
        assert!(near(pixel(&image, 2, 2), color, 2), "frame {i}");
    }
    // An animated WebP.
    let s = source("frames.webp");
    // SAFETY: a source.
    unsafe { assert_eq!((type_of(&s).as_deref(), s.count()), (Some("org.webmproject.webp"), 2)) };
    let webp = sub(&file_props(&s), "{WebP}");
    assert_eq!((number(&webp, "LoopCount"), number(&webp, "CanvasPixelHeight")), (0.0, 4.0));
    let w = sub(&props(&s, 1), "{WebP}");
    assert_eq!((number(&w, "DelayTime"), number(&w, "UnclampedDelayTime")), (0.25, 0.25));
    // SAFETY: a source.
    let blue = unsafe { s.image_at_index(1, None) }.expect("the second frame");
    assert!(near(pixel(&blue, 1, 1), BLUE, 2));
    // A still WebP: one frame, and no {WebP} of its own.
    let s = source("small.webp");
    let webp = sub(&file_props(&s), "{WebP}");
    assert_eq!((number(&webp, "LoopCount"), numbers_len(&webp, "FrameInfo")), (1.0, 1));
    assert!(value(&props(&s, 0), "{WebP}").is_none());
}

fn numbers_len(d: &CFDictionary, key: &str) -> usize {
    let v = value(d, key).unwrap_or_else(|| panic!("{key}"));
    v.downcast::<CFArray>().map(|a| a.len()).unwrap_or(0)
}

#[test]
fn unknown_and_unreadable_data() {
    // SAFETY: sources.
    unsafe {
        let s = source_of(b"hello, world: not an image file");
        assert_eq!((type_of(&s), s.count()), (None, 0));
        assert_eq!(s.status(), CGImageSourceStatus::StatusInvalidData);
        assert_eq!(s.status_at_index(0), CGImageSourceStatus::StatusUnknownType);
        assert!(s.properties(None).is_none() && s.properties_at_index(0, None).is_none());
        assert!(s.image_at_index(0, None).is_none() && s.thumbnail_at_index(0, None).is_none());
        let s = source_of(b"");
        assert_eq!(s.status_at_index(0), CGImageSourceStatus::StatusUnexpectedEOF);
        // A PNG by its signature that isn't one: an image with no
        // properties, and no pixels.
        let s = source("garbage.png");
        assert_eq!((type_of(&s).as_deref(), s.count()), (Some("public.png"), 1));
        assert_eq!(props(&s, 0).count(), 0);
        assert!(s.image_at_index(0, None).is_none());
        assert_eq!(s.status_at_index(1), CGImageSourceStatus::StatusInvalidData);
        // A TIFF too large to read: none.
        let s = source("huge.tiff");
        assert_eq!((type_of(&s).as_deref(), s.count()), (Some("public.tiff"), 0));
        assert!(s.image_at_index(0, None).is_none());
        // Past the last image.
        assert!(source("rgba-72dpi.png").properties_at_index(1, None).is_none());
        // A file that isn't there makes no source.
        let url = CFURL::from_file_path("/nonexistent/sidestep/x.png").expect("a URL");
        assert!(CGImageSource::with_url(&url, None).is_none());
    }
}

#[test]
fn sources_from_urls_and_providers() {
    let url = CFURL::from_file_path(format!("{FIXTURES}/small.gif")).expect("a URL");
    // SAFETY: plain calls.
    unsafe {
        let s = CGImageSource::with_url(&url, None).expect("a source");
        assert_eq!((type_of(&s).as_deref(), s.count()), (Some("com.compuserve.gif"), 1));
        let provider = CGDataProvider::with_url(Some(&url)).expect("a provider");
        let s = CGImageSource::with_data_provider(&provider, None).expect("a source");
        assert_eq!(type_of(&s).as_deref(), Some("com.compuserve.gif"));
    }
}

#[test]
fn images_at_index() {
    let s = source("rgba-72dpi.png");
    // SAFETY: a source.
    let image = unsafe { s.image_at_index(0, None) }.expect("an image");
    assert_eq!((CGImage::width(Some(&image)), CGImage::height(Some(&image))), (4, 2));
    assert_eq!(CGImage::bits_per_component(Some(&image)), 8);
    assert_eq!(CGImage::bits_per_pixel(Some(&image)), 32);
    // Alpha straight, last, as the file has it.
    assert_eq!(CGImage::alpha_info(Some(&image)), CGImageAlphaInfo::Last);
    assert_eq!(CGImage::ut_type(Some(&image)).map(|t| t.to_string()).as_deref(), Some("public.png"));
    assert!(near(pixel(&image, 0, 0), RED, 2));
    assert!(near(pixel(&image, 2, 0), BLUE, 2));
    assert!(near(pixel(&image, 0, 1), [128, 0, 0, 128], 2));
    // A JPEG: padding last; stored sideways, it stays as stored.
    // SAFETY: a source.
    let image = unsafe { source("orientation-6.jpg").image_at_index(0, None) }.expect("an image");
    assert_eq!((CGImage::width(Some(&image)), CGImage::height(Some(&image))), (4, 2));
    assert_eq!(CGImage::alpha_info(Some(&image)), CGImageAlphaInfo::NoneSkipLast);
    // SAFETY: a source.
    let image = unsafe { source("small.gif").image_at_index(0, None) }.expect("an image");
    assert_eq!(CGImage::ut_type(Some(&image)).map(|t| t.to_string()).as_deref(), Some("com.compuserve.gif"));
    // Decoded now, drawn the same.
    let now = [(unsafe { kCGImageSourceShouldCacheImmediately }, yes())];
    // SAFETY: a source.
    let image = unsafe { source("rgba-72dpi.png").image_at_index(0, Some(&dict(&now))) }.expect("an image");
    assert!(near(pixel(&image, 1, 0), GREEN, 2));
}

#[test]
fn sources_cache_their_images() {
    let s = source("halves.png");
    let ptr = |i: &CFRetained<CGImage>| CFRetained::as_ptr(i);
    // SAFETY: a source.
    unsafe {
        let a = s.image_at_index(0, None).expect("an image");
        let b = s.image_at_index(0, None).expect("an image");
        assert_eq!(ptr(&a), ptr(&b), "the same image, cached");
        // A thumbnail from the image, with no size or transform, is it.
        let always = [(kCGImageSourceCreateThumbnailFromImageAlways, yes())];
        let t = s.thumbnail_at_index(0, Some(&dict(&always))).expect("a thumbnail");
        assert_eq!(ptr(&a), ptr(&t));
        // Still the same while it lives.
        s.remove_cache_at_index(0);
        let c = s.image_at_index(0, None).expect("an image");
        assert_eq!(ptr(&a), ptr(&c));
        // Not cached, but alive: the same too.
        let uncached = [(kCGImageSourceShouldCache, no())];
        let d = s.image_at_index(0, Some(&dict(&uncached))).expect("an image");
        assert_eq!(ptr(&c), ptr(&d));
        // Another source's is another image.
        let other = source("halves.png").image_at_index(0, None).expect("an image");
        assert_ne!(ptr(&c), ptr(&other));
    }
}

fn thumb(name: &str, options: &[(&CFString, &CFType)]) -> Option<CFRetained<CGImage>> {
    // SAFETY: a source, and options of ImageIO's keys.
    unsafe { source(name).thumbnail_at_index(0, Some(&dict(options))) }
}

fn size(image: &CGImage) -> (usize, usize) {
    (CGImage::width(Some(image)), CGImage::height(Some(image)))
}

#[test]
fn thumbnails() {
    // SAFETY: the keys are ImageIO's.
    let (always, if_absent, max, transform) = unsafe {
        (
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailFromImageIfAbsent,
            kCGImageSourceThumbnailMaxPixelSize,
            kCGImageSourceCreateThumbnailWithTransform,
        )
    };
    let n = |v: f64| CFNumber::new_f64(v);
    let (n16, n16_7, n100, n6) = (n(16.0), n(16.7), n(100.0), n(6.0));
    // Halved while half the longer side still reaches the size (rounded
    // down), then scaled to it: 48 × 29 is 24 × 14, then 16 × 9.
    let t = thumb("wide.png", &[(max, cf(&*n16))]).expect("a thumbnail");
    assert_eq!(size(&t), (16, 9));
    assert_eq!(size(&thumb("wide.png", &[(max, cf(&*n16_7))]).expect("a thumbnail")), (16, 9));
    // Premultiplied, alpha first.
    assert_eq!(CGImage::alpha_info(Some(&t)), CGImageAlphaInfo::PremultipliedFirst);
    assert!(near(pixel(&t, 2, 4), RED, 8) && near(pixel(&t, 8, 4), GREEN, 8) && near(pixel(&t, 13, 4), BLUE, 8));
    // Never larger than the image; the whole image without a size.
    assert_eq!(size(&thumb("wide.png", &[(max, cf(&*n100))]).expect("a thumbnail")), (48, 29));
    assert_eq!(size(&thumb("wide.png", &[]).expect("a thumbnail")), (48, 29));
    // A JPEG with an EXIF thumbnail (30 × 20, green) uses it for a size,
    // made smaller, never larger, unless asked for one from the image.
    let t = thumb("exif-thumb.jpg", &[(max, cf(&*n100))]).expect("a thumbnail");
    assert_eq!(size(&t), (30, 20));
    assert!(near(pixel(&t, 15, 10), GREEN, 12));
    assert_eq!(CGImage::alpha_info(Some(&t)), CGImageAlphaInfo::NoneSkipFirst);
    assert_eq!(size(&thumb("exif-thumb.jpg", &[(max, cf(&*n6))]).expect("a thumbnail")), (6, 4));
    let t = thumb("exif-thumb.jpg", &[(always, yes()), (max, cf(&*n16))]).expect("a thumbnail");
    assert_eq!(size(&t), (16, 11));
    assert!(near(pixel(&t, 1, 5), RED, 12));
    // Without a size, the whole image.
    assert_eq!(size(&thumb("exif-thumb.jpg", &[(if_absent, yes())]).expect("a thumbnail")), (300, 200));
    // Turned upright by the orientation (6: a quarter turn clockwise), the
    // embedded thumbnail too.
    let t = thumb("exif-thumb.jpg", &[(always, yes()), (max, cf(&*n16)), (transform, yes())]).expect("a thumbnail");
    assert_eq!(size(&t), (11, 16));
    // The stored image's left (red) is now its top.
    assert!(near(pixel(&t, 5, 1), RED, 12) && near(pixel(&t, 5, 14), BLUE, 12));
    let t = thumb("exif-thumb.jpg", &[(max, cf(&*n6)), (transform, yes())]).expect("a thumbnail");
    assert_eq!(size(&t), (4, 6));
    assert!(near(pixel(&t, 2, 3), GREEN, 12));
    assert_eq!(size(&thumb("exif-thumb.jpg", &[(transform, yes())]).expect("a thumbnail")), (200, 300));
    // A JPEG without one, asked not to make one from the image, has none;
    // a PNG makes one anyway.
    assert!(thumb("plain.jpg", &[(always, no()), (max, cf(&*n16))]).is_none());
    assert_eq!(size(&thumb("plain.jpg", &[(always, no()), (if_absent, yes()), (max, cf(&*n16))]).unwrap()), (16, 11));
    assert_eq!(size(&thumb("plain.jpg", &[(max, cf(&*n16))]).expect("a thumbnail")), (16, 11));
    assert_eq!(size(&thumb("wide.png", &[(always, no()), (max, cf(&*n16))]).expect("a thumbnail")), (16, 9));
    // Of an image with alpha: premultiplied.
    let t = thumb("rgba-72dpi.png", &[(max, cf(&*n100))]).expect("a thumbnail");
    assert!(near(pixel(&t, 0, 1), [128, 0, 0, 128], 3));
}

#[test]
fn incremental_sources() {
    let file = fixture("rgba-72dpi.png");
    // SAFETY: plain calls.
    unsafe {
        let s = CGImageSource::new_incremental(None);
        assert_eq!((type_of(&s), s.count(), s.status()), (None, 0, CGImageSourceStatus::StatusInvalidData));
        s.update_data(&CFData::from_bytes(&file[..20]), false);
        assert_eq!(type_of(&s).as_deref(), Some("public.png"));
        assert_eq!((s.count(), s.status()), (1, CGImageSourceStatus::StatusIncomplete));
        assert_eq!(s.status_at_index(0), CGImageSourceStatus::StatusIncomplete);
        s.update_data(&CFData::from_bytes(&file[..file.len() / 2]), false);
        assert!(s.image_at_index(0, None).is_none());
        s.update_data(&CFData::from_bytes(&file), true);
        assert_eq!(s.status(), CGImageSourceStatus::StatusComplete);
        assert_eq!(s.status_at_index(0), CGImageSourceStatus::StatusComplete);
        let image = s.image_at_index(0, None).expect("the whole image");
        assert_eq!(size(&image), (4, 2));
        assert_eq!(number(&props(&s, 0), "PixelWidth"), 4.0);
    }
}

/// A `w` × `h` image of red, green and blue thirds, half transparent in its
/// bottom half if `alpha`.
fn made_image(w: usize, h: usize, alpha: bool) -> CFRetained<CGImage> {
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).expect("sRGB");
    // SAFETY: no data: the context allocates.
    let ctx = unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, Some(&space), CGImageAlphaInfo::PremultipliedLast.0)
    }
    .expect("a context");
    for (i, c) in [RED, GREEN, BLUE].into_iter().enumerate() {
        let f = |v: u8| f64::from(v) / 255.0;
        CGContext::set_rgb_fill_color(Some(&ctx), f(c[0]), f(c[1]), f(c[2]), 1.0);
        let x = (i * w / 3) as f64;
        CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::new(x, 0.0), CGSize::new((w / 3) as f64, h as f64)));
    }
    if alpha {
        CGContext::clear_rect(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, (h / 2) as f64)));
    }
    CGBitmapContextCreateImage(Some(&ctx)).expect("an image")
}

/// The data a destination of `kind` writes of `images` (each with its
/// properties) and the file's properties, and whether it finalized.
fn write(
    kind: &str,
    count: usize,
    images: &[(&CGImage, Option<&CFDictionary>)],
    file: Option<&CFDictionary>,
) -> (bool, Vec<u8>) {
    let data = CFMutableData::new(None, 0).expect("data");
    // SAFETY: plain calls.
    unsafe {
        let d = CGImageDestination::with_data(&data, &CFString::from_str(kind), count, None).expect("a destination");
        assert_eq!(CFGetTypeID(Some(cf(&*d))), CGImageDestination::type_id());
        d.set_properties(file);
        for (image, props) in images {
            d.add_image(image, *props);
        }
        let ok = d.finalize();
        (ok, CFData::to_vec(&data))
    }
}

#[test]
fn destinations_write_files_sources_read() {
    let image = made_image(24, 12, true);
    // SAFETY: the keys are ImageIO's.
    let (dpi_w, dpi_h, quality) =
        unsafe { (kCGImagePropertyDPIWidth, kCGImagePropertyDPIHeight, kCGImageDestinationLossyCompressionQuality) };
    let n144 = CFNumber::new_f64(144.0);
    let at144 = dict(&[(dpi_w, cf(&*n144)), (dpi_h, cf(&*n144))]);
    // PNG: alpha kept, density as pHYs.
    let (ok, png) = write("public.png", 1, &[(&image, Some(&at144))], None);
    assert!(ok);
    let s = source_of(&png);
    assert_eq!(type_of(&s).as_deref(), Some("public.png"));
    let p = props(&s, 0);
    assert_eq!((number(&p, "PixelWidth"), number(&p, "PixelHeight")), (24.0, 12.0));
    assert_eq!((flag(&p, "HasAlpha"), number(&p, "DPIWidth")), (Some(true), 144.0));
    // SAFETY: a source.
    let back = unsafe { s.image_at_index(0, None) }.expect("an image");
    assert!(near(pixel(&back, 2, 2), RED, 2) && near(pixel(&back, 12, 2), GREEN, 2));
    assert_eq!(pixel(&back, 20, 10)[3], 0, "the transparent half");
    // JPEG: quality and density.
    let opaque = made_image(24, 12, false);
    let q = |v: f64| CFNumber::new_f64(v);
    let (low, high) = (q(0.1), q(1.0));
    let (ok, small) = write("public.jpeg", 1, &[(&opaque, Some(&dict(&[(quality, cf(&*low))])))], None);
    assert!(ok);
    let (_, large) = write("public.jpeg", 1, &[(&opaque, Some(&dict(&[(quality, cf(&*high))])))], None);
    assert!(small.len() < large.len(), "{} < {}", small.len(), large.len());
    let (_, jpeg) = write("public.jpeg", 1, &[(&opaque, Some(&at144))], None);
    let s = source_of(&jpeg);
    assert_eq!(type_of(&s).as_deref(), Some("public.jpeg"));
    assert_eq!(number(&props(&s, 0), "DPIWidth"), 144.0);
    // SAFETY: a source.
    let back = unsafe { s.image_at_index(0, None) }.expect("an image");
    assert!(near(pixel(&back, 20, 6), BLUE, 12));
    // TIFF and BMP.
    for kind in ["public.tiff", "com.microsoft.bmp"] {
        let (ok, bytes) = write(kind, 1, &[(&opaque, None)], None);
        assert!(ok, "{kind}");
        let s = source_of(&bytes);
        assert_eq!(type_of(&s).as_deref(), Some(kind));
        // SAFETY: a source.
        let back = unsafe { s.image_at_index(0, None) }.expect("an image");
        assert!(near(pixel(&back, 12, 6), GREEN, 2), "{kind}");
    }
}

#[test]
fn animated_gifs_are_written() {
    let image = made_image(12, 6, false);
    // SAFETY: the keys are ImageIO's.
    let (gif_key, delay_key, loop_key) =
        unsafe { (kCGImagePropertyGIFDictionary, kCGImagePropertyGIFDelayTime, kCGImagePropertyGIFLoopCount) };
    let frames: Vec<CFRetained<CFDictionary>> = [0.5, 1.5, 2.5]
        .iter()
        .map(|d| {
            let delay = CFNumber::new_f64(*d);
            let inner = dict(&[(delay_key, cf(&*delay))]);
            dict(&[(gif_key, cf(&*inner))])
        })
        .collect();
    let five = CFNumber::new_i32(5);
    let file = dict(&[(gif_key, cf(&*dict(&[(loop_key, cf(&*five))])))]);
    let images: Vec<(&CGImage, Option<&CFDictionary>)> = frames.iter().map(|f| (&*image, Some(&**f))).collect();
    let (ok, gif) = write("com.compuserve.gif", 3, &images, Some(&file));
    assert!(ok);
    let s = source_of(&gif);
    // SAFETY: a source.
    unsafe { assert_eq!((type_of(&s).as_deref(), s.count()), (Some("com.compuserve.gif"), 3)) };
    let g = sub(&file_props(&s), "{GIF}");
    assert_eq!(number(&g, "LoopCount"), 5.0);
    for (i, d) in [0.5, 1.5, 2.5].into_iter().enumerate() {
        assert!(close(number(&sub(&props(&s, i), "{GIF}"), "DelayTime"), d), "frame {i}");
    }
    // From a source: its frames and delays.
    let from = source("frames.gif");
    let data = CFMutableData::new(None, 0).expect("data");
    // SAFETY: plain calls.
    unsafe {
        let d = CGImageDestination::with_data(&data, &CFString::from_str("com.compuserve.gif"), 3, None)
            .expect("a destination");
        for i in 0..3 {
            d.add_image_from_source(&from, i, None);
        }
        assert!(d.finalize());
    }
    let s = source_of(&CFData::to_vec(&data));
    // SAFETY: a source.
    unsafe { assert_eq!(s.count(), 3) };
    assert!(close(number(&sub(&props(&s, 2), "{GIF}"), "DelayTime"), 0.3));
    // SAFETY: a source.
    let green = unsafe { s.image_at_index(1, None) }.expect("a frame");
    assert!(near(pixel(&green, 1, 1), GREEN, 2));
}

#[test]
fn finalizing_needs_the_images_promised() {
    let image = made_image(6, 3, false);
    // Fewer than promised is fine; none, or more, writes nothing.
    assert!(write("public.png", 2, &[(&image, None)], None).0);
    let (ok, bytes) = write("public.png", 1, &[(&image, None), (&image, None)], None);
    assert!(!ok && bytes.is_empty());
    assert!(!write("public.png", 1, &[], None).0);
    let data = CFMutableData::new(None, 0).expect("data");
    // SAFETY: plain calls.
    unsafe {
        let d =
            CGImageDestination::with_data(&data, &CFString::from_str("public.png"), 1, None).expect("a destination");
        d.add_image(&image, None);
        assert!(d.finalize());
        assert!(!d.finalize(), "only once");
        // A type not written makes no destination.
        let bogus = CGImageDestination::with_data(&data, &CFString::from_str("public.sidestep-bogus"), 1, None);
        assert!(bogus.is_none());
    }
    // To a file.
    let path = std::env::temp_dir().join(format!("sidestep-imageio-{}.png", std::process::id()));
    let url = CFURL::from_file_path(&path).expect("a URL");
    // SAFETY: plain calls.
    unsafe {
        let d = CGImageDestination::with_url(&url, &CFString::from_str("public.png"), 1, None).expect("a destination");
        d.add_image(&image, None);
        assert!(d.finalize());
    }
    let s = source_of(&std::fs::read(&path).expect("the file"));
    let _ = std::fs::remove_file(&path);
    assert_eq!(number(&props(&s, 0), "PixelWidth"), 6.0);
}

#[test]
fn frames_asked_for_out_of_order() {
    let s = source("frames.gif");
    // SAFETY: a source.
    unsafe {
        let two = s.image_at_index(2, None).expect("frame 2");
        let zero = s.image_at_index(0, None).expect("frame 0");
        let one = s.image_at_index(1, None).expect("frame 1");
        assert!(near(pixel(&two, 2, 2), BLUE, 2), "frame 2");
        assert!(near(pixel(&zero, 2, 2), RED, 2), "frame 0");
        assert!(near(pixel(&one, 2, 2), GREEN, 2), "frame 1");
        let ptrs = [CFRetained::as_ptr(&zero), CFRetained::as_ptr(&one), CFRetained::as_ptr(&two)];
        assert!(ptrs[0] != ptrs[1] && ptrs[1] != ptrs[2] && ptrs[0] != ptrs[2], "three images");
        // Each is the one its index gives from then on.
        assert_eq!(CFRetained::as_ptr(&s.image_at_index(0, None).expect("frame 0")), ptrs[0]);
        assert_eq!(CFRetained::as_ptr(&s.image_at_index(2, None).expect("frame 2")), ptrs[2]);
    }
}

#[test]
fn thumbnail_sizes() {
    // SAFETY: the keys are ImageIO's.
    let (always, max) = unsafe { (kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceThumbnailMaxPixelSize) };
    // Image size and the size asked for, then the thumbnail's size from a
    // PNG and from a JPEG, which halve differently (a JPEG while half its
    // longer side, to the nearest pixel, reaches the size; a PNG while it
    // does rounded down), and round the shorter side's half pixel to even.
    let cases = [
        ((129, 64), 65.0, (65, 32), (64, 32)),
        ((30, 20), 4.0, (4, 3), (3, 2)),
        ((400, 200), 69.0, (69, 34), (69, 34)),
        ((48, 29), 16.0, (16, 9), (16, 9)),
    ];
    for ((w, h), size, png, jpeg) in cases {
        let image = made_image(w, h, false);
        let n = CFNumber::new_f64(size);
        let options = dict(&[(always, yes()), (max, cf(&*n))]);
        for (kind, want) in [("public.png", png), ("public.jpeg", jpeg)] {
            let (ok, bytes) = write(kind, 1, &[(&image, None)], None);
            assert!(ok, "{kind}");
            // SAFETY: a source and ImageIO's options.
            let t = unsafe { source_of(&bytes).thumbnail_at_index(0, Some(&options)) }.expect("a thumbnail");
            assert_eq!(self::size(&t), want, "{w} × {h} in {size} from a {kind}");
        }
    }
}

/// A 24-bit BMP `w` × `h`, red, with its info header's pixels per meter.
fn bmp(w: usize, h: usize, ppm: (i32, i32)) -> Vec<u8> {
    let row = (w * 3).div_ceil(4) * 4;
    let mut pixels = Vec::new();
    for _ in 0..h {
        let mut r: Vec<u8> = [0u8, 0, 255].repeat(w);
        r.resize(row, 0);
        pixels.extend(r);
    }
    let mut out = b"BM".to_vec();
    out.extend((54 + pixels.len() as u32).to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(54u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend((w as i32).to_le_bytes());
    out.extend((h as i32).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(24u16.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend((pixels.len() as u32).to_le_bytes());
    out.extend(ppm.0.to_le_bytes());
    out.extend(ppm.1.to_le_bytes());
    out.extend([0u8; 8]);
    out.extend(pixels);
    out
}

#[test]
fn bmp_densities() {
    let dpi = |ppm: (i32, i32)| {
        let p = props(&source_of(&bmp(4, 2, ppm)), 0);
        value(&p, "DPIWidth").map(|_| (typed(&p, "DPIWidth"), number(&p, "DPIHeight")))
    };
    // Pixels per meter as dots per inch; within a twentieth of 72 or 96,
    // those.
    assert_eq!(dpi((2835, 2835)), Some(((CFNumberType::Float32Type, 72.0), 72.0)));
    assert_eq!(dpi((2833, 2836)), Some(((CFNumberType::Float32Type, 72.0), 72.0)));
    assert_eq!(dpi((3780, 2835)).map(|((_, x), y)| (x, y)), Some((96.0, 72.0)));
    let (x, y) = dpi((5669, 11811)).map(|((_, x), y)| (x, y)).expect("a density");
    assert!((x - 143.9926).abs() < 1e-3 && (y - 299.9994).abs() < 1e-3, "{x} {y}");
    assert!((dpi((2837, 2837)).expect("a density").0.1 - 72.0598).abs() < 1e-3);
    // None under ten dots per inch, either way.
    assert_eq!(dpi((0, 0)), None);
    assert_eq!(dpi((3780, 393)), None);
    assert!(dpi((394, 394)).is_some());
}

/// An icon file of one icon, `payload`, its directory saying `w` × `h`.
fn ico_of(w: u8, h: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 1, 0, 1, 0, w, h, 0, 0, 1, 0, 32, 0];
    out.extend((payload.len() as u32).to_le_bytes());
    out.extend(22u32.to_le_bytes());
    out.extend(payload);
    out
}

/// An icon file of a red 32-bit bitmap `w` × `h`.
fn ico_bitmap(w: u32, h: u32) -> Vec<u8> {
    let xor = [0u8, 0, 255, 255].repeat((w * h) as usize);
    let and = vec![0u8; (w.div_ceil(32) * 4 * h) as usize];
    let mut dib = Vec::new();
    dib.extend(40u32.to_le_bytes());
    dib.extend((w as i32).to_le_bytes());
    dib.extend((h as i32 * 2).to_le_bytes());
    dib.extend(1u16.to_le_bytes());
    dib.extend(32u16.to_le_bytes());
    dib.extend(0u32.to_le_bytes());
    dib.extend(((xor.len() + and.len()) as u32).to_le_bytes());
    dib.extend([0u8; 16]);
    dib.extend(xor);
    dib.extend(and);
    ico_of(w as u8, h as u8, &dib)
}

#[test]
fn icon_files() {
    // SAFETY: sources.
    unsafe {
        let s = source_of(&ico_bitmap(16, 16));
        assert_eq!((type_of(&s).as_deref(), s.count()), (Some("com.microsoft.ico"), 1));
        assert_eq!((number(&props(&s, 0), "PixelWidth"), flag(&props(&s, 0), "HasAlpha")), (16.0, Some(true)));
        assert_eq!(type_of(&source_of(&ico_bitmap(12, 13))).as_deref(), Some("com.microsoft.ico"));
        // One whose icon is smaller than 12 pixels either way isn't read.
        for (w, h) in [(4, 2), (11, 11), (16, 8), (8, 16)] {
            let s = source_of(&ico_bitmap(w, h));
            assert_eq!((type_of(&s), s.count()), (None, 0), "{w} × {h}");
            assert_eq!(s.status(), CGImageSourceStatus::StatusInvalidData);
        }
        // An icon stored as a PNG has the PNG's dictionary.
        let (_, png) = write("public.png", 1, &[(&made_image(24, 12, true), None)], None);
        let s = source_of(&ico_of(24, 12, &png));
        assert_eq!(type_of(&s).as_deref(), Some("com.microsoft.ico"));
        let p = props(&s, 0);
        assert_eq!((number(&p, "PixelWidth"), number(&p, "PixelHeight")), (24.0, 12.0));
        assert_eq!(number(&sub(&p, "{PNG}"), "InterlaceType"), 0.0);
    }
}

#[test]
fn iptc_from_other_metadata() {
    // A PNG's title and description.
    let iptc = sub(&props(&source("text.png"), 0), "{IPTC}");
    assert_eq!((string(&iptc, "ObjectName"), string(&iptc, "Caption/Abstract")), ("A title".into(), "Café".into()));
    // A JPEG's image description.
    let p = props(&source("exif-thumb.jpg"), 0);
    assert_eq!(string(&sub(&p, "{TIFF}"), "ImageDescription"), "A test");
    assert_eq!(string(&sub(&p, "{IPTC}"), "Caption/Abstract"), "A test");
    // Nothing to make one of: none.
    assert!(value(&props(&source("rgba-72dpi.png"), 0), "{IPTC}").is_none());
}
