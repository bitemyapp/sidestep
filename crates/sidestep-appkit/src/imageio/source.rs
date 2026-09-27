//! `CGImageSource`: an image file's images, properties and thumbnails.
//!
//! A source keeps the file's bytes and what [`inspect`] read of them (the
//! header and metadata only). Images are `CGImage`s over the codecs
//! (`crate::codec`): the first image of a file keeps the file and is
//! decoded where it's drawn (the render thread, for windows), as `NSImage`'s
//! are, unless `kCGImageSourceShouldCacheImmediately` asks for its pixels
//! now; the later frames of an animated GIF or WebP are decoded when they
//! are made, on the caller's thread, with nothing of the source locked but
//! the frame decoder (so another thread may ask for the count, properties
//! or another image meanwhile). A source hands out the same image for
//! an index while anything keeps it, and keeps it itself while caching
//! (`kCGImageSourceShouldCache`, true unless said otherwise, as on 64-bit
//! macOS) until `CGImageSourceRemoveCacheAtIndex`, as ImageIO does.
//!
//! Thumbnails follow what ImageIO does on macOS (measured):
//!
//! - With no `kCGImageSourceThumbnailMaxPixelSize`, the thumbnail is the
//!   whole image; with `kCGImageSourceCreateThumbnailFromImageAlways` and
//!   no size or transform, it's the image itself.
//! - With a size, a JPEG's EXIF thumbnail is used unless
//!   `…FromImageAlways` is true: made smaller to fit, never larger. (Only
//!   one with the image's proportions; ImageIO passes over some others, of
//!   small files among them, for reasons of its own.)
//!   Without one, the thumbnail is made from the image, except that a JPEG
//!   or TIFF asked with `…FromImageAlways` false (and not
//!   `…FromImageIfAbsent`) has none.
//! - The size bounds the longer side, as a whole number of pixels (the size
//!   given, rounded down): the image is halved while half its longer side
//!   still reaches it (rounded down, or to the nearest pixel for a JPEG),
//!   then scaled down to it, the shorter side to the nearest pixel (see
//!   [`fitted`]); an image already that small keeps its size.
//! - `kCGImageSourceCreateThumbnailWithTransform` turns the thumbnail
//!   upright by the image's EXIF orientation (an embedded thumbnail too).
//! - Thumbnails are premultiplied 8-bit sRGB with alpha first (or padding
//!   first, for images without alpha), made on the caller's thread.
//!
//! What isn't read: HEIC, AVIF, RAW and the other types the codecs don't
//! have (a source of one has no type and no images, as ImageIO's of an
//! unknown type); more than one page of a TIFF, or image of an ICO, or
//! frame of an animated PNG (each reads as its first); metadata trees
//! (`CGImageSourceCopyMetadataAtIndex` gives NULL), auxiliary data, and
//! `kCGImageSourceSubsampleFactor` (ignored).

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFArray, CFData, CFDictionary, CFString, CFTypeID, CFURL};
use objc2_core_graphics::{CGDataProvider, CGImage, CGImageAlphaInfo};
use objc2_foundation::NSString;
use objc2_image_io::{
    CGImageSource, CGImageSourceStatus, kCGImageSourceCreateThumbnailFromImageAlways,
    kCGImageSourceCreateThumbnailFromImageIfAbsent, kCGImageSourceCreateThumbnailWithTransform,
    kCGImageSourceShouldCache, kCGImageSourceShouldCacheImmediately, kCGImageSourceThumbnailMaxPixelSize,
};

use super::format::Kind;
use super::inspect::{Info, inspect};
use super::plist::{Dict, P, read};
use crate::coregraphics::image::{CGImageImpl, from_file_of_type, from_worked_out};

struct State {
    data: Arc<[u8]>,
    /// All the data is there (always, but for an incremental source).
    complete: bool,
    /// What was read of the data, if it's a file of a type read here.
    info: Option<Arc<Info>>,
    /// The images made, by index: kept while caching, and known while
    /// anything else keeps them (a source hands out the same image while
    /// it lives, after `CGImageSourceRemoveCacheAtIndex` too, as ImageIO
    /// does).
    images: Vec<Made>,
}

/// An image made at an index: kept (while caching), and known (while
/// anything keeps it); neither for an index not made.
type Made = (Option<Retained<CGImageImpl>>, Option<Weak<CGImageImpl>>);

pub(crate) struct SourceIvars {
    /// Held only to read or record what's known, never while decoding, so
    /// that a thread making a thumbnail doesn't hold up another asking for
    /// the count or the properties.
    state: Mutex<State>,
    /// The decoder of an animated file's later frames, kept between them
    /// (it knows the data it decodes): held while a frame is decoded, and
    /// never together with `state`.
    frames: Mutex<Option<crate::codec::Animation>>,
    /// Images are cached unless the creation options said not to.
    cache: bool,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the state is behind
    // a mutex, since a source may be used from any thread.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGImageSource"]
    #[ivars = SourceIvars]
    pub(crate) struct CGImageSourceImpl;

    impl CGImageSourceImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let kind = self.state().info.as_ref().map(|i| i.kind.uti());
            let rest = format!("[{}]", kind.unwrap_or("unknown"));
            crate::coregraphics::description("CGImageSource", self, &rest)
        }
    }

    unsafe impl NSObjectProtocol for CGImageSourceImpl {}
);

pub(crate) fn source_imp(s: &CGImageSource) -> &CGImageSourceImpl {
    // SAFETY: every CGImageSource is a CGImageSourceImpl.
    unsafe { &*(s as *const CGImageSource).cast::<CGImageSourceImpl>() }
}

fn make(data: Arc<[u8]>, complete: bool, options: Option<&CFDictionary>) -> Retained<CGImageSourceImpl> {
    let info = if complete { inspect(&data).map(Arc::new) } else { None };
    let cache = read::flag(read::dict(options), unsafe { kCGImageSourceShouldCache }).unwrap_or(true);
    let mut state = State { data, complete, info, images: Vec::new() };
    if !complete {
        refresh(&mut state);
    }
    let ivars = SourceIvars { state: Mutex::new(state), frames: Mutex::new(None), cache };
    let this = CGImageSourceImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// Read what an incremental source's data holds so far.
fn refresh(state: &mut State) {
    state.info = if state.complete {
        inspect(&state.data).map(Arc::new)
    } else {
        // Until the last of the data, the file's images are known as far
        // as its header goes; one while even that is still coming.
        Kind::sniff(&state.data).map(|kind| {
            let mut info = inspect(&state.data).unwrap_or_else(|| Info {
                kind,
                count: 0,
                images: Vec::new(),
                file: Dict::new(),
                thumbnail: None,
                animated: false,
            });
            info.count = info.count.max(1);
            Arc::new(info)
        })
    };
    state.images.clear();
}

impl CGImageSourceImpl {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.ivars().state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn info(&self) -> Option<Arc<Info>> {
        self.state().info.clone()
    }

    pub(crate) fn kind(&self) -> Option<Kind> {
        self.info().map(|i| i.kind)
    }

    fn count(&self) -> usize {
        self.info().map_or(0, |i| i.count)
    }

    /// The image at `index`, made (or found) as the options say. Pixels are
    /// decoded with the source's state unlocked.
    pub(crate) fn image_at(&self, index: usize, options: Option<&CFDictionary>) -> Option<Retained<CGImageImpl>> {
        let o = read::dict(options);
        let cache = read::flag(o, unsafe { kCGImageSourceShouldCache }).unwrap_or(self.ivars().cache);
        let now = read::flag(o, unsafe { kCGImageSourceShouldCacheImmediately }).unwrap_or(false);
        let (info, data) = {
            let mut state = self.state();
            if !state.complete {
                return None;
            }
            let info = state.info.clone()?;
            info.images.get(index)?;
            if let Some(image) = known(&mut state, index, cache) {
                return Some(image);
            }
            (info, state.data.clone())
        };
        let facts = &info.images[index];
        let file_type = Some(info.kind.constant());
        let image = if index == 0 && !now && !info.animated {
            from_file_of_type(data.clone(), false, true, file_type)?
        } else {
            let straight = self.straight_pixels(&data, &info, index)?;
            straight_image(facts.width as usize, facts.height as usize, straight, facts.alpha, file_type)?
        };
        let mut state = self.state();
        // Made by another thread meanwhile: that one, the one known.
        if let Some(image) = known(&mut state, index, cache) {
            return Some(image);
        }
        // Kept unless the data changed meanwhile (an incremental source's).
        if Arc::ptr_eq(&state.data, &data) {
            if state.images.len() <= index {
                state.images.resize_with(index + 1, || (None, None));
            }
            state.images[index] = (cache.then(|| image.clone()), Some(Weak::from_retained(&image)));
        }
        Some(image)
    }

    /// A thumbnail of the image at `index`, as the options say (see the
    /// module's documentation).
    pub(crate) fn thumbnail_at(&self, index: usize, options: Option<&CFDictionary>) -> Option<Retained<CGImageImpl>> {
        let o = read::dict(options);
        // SAFETY: the keys are constants this module exports.
        let (always, if_absent, max, transform, max_given, transform_given) = unsafe {
            (
                read::flag(o, kCGImageSourceCreateThumbnailFromImageAlways),
                read::flag(o, kCGImageSourceCreateThumbnailFromImageIfAbsent),
                read::number(o, kCGImageSourceThumbnailMaxPixelSize).filter(|m| *m >= 1.0).map(|m| m.floor() as u32),
                read::flag(o, kCGImageSourceCreateThumbnailWithTransform).unwrap_or(false),
                read::value(o, kCGImageSourceThumbnailMaxPixelSize).is_some(),
                read::value(o, kCGImageSourceCreateThumbnailWithTransform).is_some(),
            )
        };
        if always == Some(true) && !max_given && !transform_given {
            return self.image_at(index, None);
        }
        let (info, data) = {
            let state = self.state();
            if !state.complete {
                return None;
            }
            (state.info.clone()?, state.data.clone())
        };
        let facts = info.images.get(index)?;
        // An EXIF thumbnail is taken if it has the image's proportions (to
        // a pixel): one that doesn't isn't of the image as it is now.
        let embedded =
            info.thumbnail.clone().filter(|_| always != Some(true) && max.is_some() && index == 0).and_then(|range| {
                let d = crate::codec::decode_straight(data.get(range)?, false)?;
                let (tw, th, iw, ih) =
                    (u64::from(d.width), u64::from(d.height), u64::from(facts.width), u64::from(facts.height));
                ((tw * ih).abs_diff(th * iw) <= iw.max(ih)).then_some((d.width, d.height, d.rgba))
            });
        let (w, h, rgba) = match embedded {
            Some(e) => e,
            None => {
                let refused = always == Some(false)
                    && if_absent != Some(true)
                    && max.is_some()
                    && matches!(info.kind, Kind::Jpeg | Kind::Tiff);
                if refused {
                    return None;
                }
                let straight = self.straight_pixels(&data, &info, index)?;
                (facts.width, facts.height, straight)
            }
        };
        let mut image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(w, h, rgba)?);
        if transform {
            image.apply_orientation(orientation(facts.orientation));
        }
        let jpeg = info.kind == Kind::Jpeg;
        if let Some((tw, th)) = max.and_then(|max| fitted(image.width(), image.height(), max, jpeg)) {
            image = image::DynamicImage::ImageRgba8(image::imageops::thumbnail(&image, tw, th));
        }
        let rgba = image.into_rgba8();
        let (w, h) = rgba.dimensions();
        argb_image(w as usize, h as usize, rgba.into_raw(), facts.alpha)
    }

    /// The straight RGBA pixels of image `index` of `data` (the source's),
    /// as stored (not turned upright): an animated file's frame as it
    /// shows, or the file's one image.
    fn straight_pixels(&self, data: &Arc<[u8]>, info: &Info, index: usize) -> Option<Vec<u8>> {
        if !info.animated {
            return crate::codec::decode_straight(data, false).map(|d| d.rgba);
        }
        let mut frames = self.ivars().frames.lock().unwrap_or_else(|e| e.into_inner());
        if frames.as_ref().is_none_or(|f| !f.decodes(data)) {
            *frames = Some(match info.kind {
                Kind::WebP => crate::codec::Animation::webp(data.clone()),
                _ => crate::codec::Animation::gif(data.clone()),
            });
        }
        frames.as_mut()?.frame(index)
    }
}

/// The size an image `w` × `h` is made to fit `max` pixels at, as ImageIO
/// sizes thumbnails (measured with PNG, BMP and JPEG files): halved
/// (rounding down) as many times as half its longer side, rounded down (to
/// the nearest pixel, for a `jpeg`), still reaches `max`, then scaled for
/// the longer side to be `max` if it's longer, the shorter side to the
/// nearest pixel (ties to even); `None` if it fits already. So 48 × 29 fits
/// 16 as 24 × 14, then 16 × 9; 30 × 20 fits 4 as 7 × 5, then 4 × 3, or as
/// 3 × 2 from a JPEG (whose halving goes on, 30 / 8 rounding to 4).
pub(crate) fn fitted(w: u32, h: u32, max: u32, jpeg: bool) -> Option<(u32, u32)> {
    if max == 0 || w.max(h) <= max {
        return None;
    }
    let long = f64::from(w.max(h));
    let half = |halvings: u32| {
        let v = long / f64::from(2u32 << halvings);
        if jpeg { v.round() } else { v.floor() }
    };
    let mut halvings = 0;
    while halvings < 31 && half(halvings) >= f64::from(max) {
        halvings += 1;
    }
    let (w, h) = ((w >> halvings).max(1), (h >> halvings).max(1));
    if w.max(h) <= max {
        return Some((w, h));
    }
    let short = (f64::from(w.min(h)) * f64::from(max) / f64::from(w.max(h))).round_ties_even();
    let short = (short as u32).max(1);
    Some(if w >= h { (max, short) } else { (short, max) })
}

/// The image made at `index`, if one is known: kept, or kept by something
/// else; kept from now on when caching.
fn known(state: &mut State, index: usize, cache: bool) -> Option<Retained<CGImageImpl>> {
    let (kept, weak) = state.images.get_mut(index)?;
    let image = kept.clone().or_else(|| weak.as_ref()?.load())?;
    if cache {
        *kept = Some(image.clone());
    }
    Some(image)
}

/// An EXIF orientation as the codecs name it.
fn orientation(o: u16) -> image::metadata::Orientation {
    image::metadata::Orientation::from_exif(o as u8).unwrap_or(image::metadata::Orientation::NoTransforms)
}

fn premultiply(p: [u8; 4]) -> [u8; 4] {
    let a = u32::from(p[3]);
    if a == 255 {
        return p;
    }
    let m = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
    [m(p[0]), m(p[1]), m(p[2]), p[3]]
}

/// An image of straight RGBA pixels, as ImageIO makes a decoded file's: 8-bit
/// RGBA, alpha last and straight, or padding last without alpha.
fn straight_image(
    w: usize,
    h: usize,
    straight: Vec<u8>,
    alpha: bool,
    file_type: Option<&'static sidestep_runtime::ObjectRef>,
) -> Option<Retained<CGImageImpl>> {
    let rgba: Vec<u8> = straight.as_chunks::<4>().0.iter().flat_map(|p| premultiply(*p)).collect();
    let info = if alpha { CGImageAlphaInfo::Last } else { CGImageAlphaInfo::NoneSkipLast };
    from_worked_out(w, h, info, Arc::from(straight), Arc::from(rgba), file_type)
}

/// A thumbnail's image, as ImageIO makes them: premultiplied, alpha first
/// (padding first without alpha).
fn argb_image(w: usize, h: usize, straight: Vec<u8>, alpha: bool) -> Option<Retained<CGImageImpl>> {
    let rgba: Vec<u8> = straight.as_chunks::<4>().0.iter().flat_map(|p| premultiply(*p)).collect();
    let argb: Vec<u8> =
        rgba.as_chunks::<4>().0.iter().flat_map(|p| [if alpha { p[3] } else { 255 }, p[0], p[1], p[2]]).collect();
    let info = if alpha { CGImageAlphaInfo::PremultipliedFirst } else { CGImageAlphaInfo::NoneSkipFirst };
    from_worked_out(w, h, info, Arc::from(argb), Arc::from(rgba), None)
}

// The functions.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_IMAGE_SOURCE
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCopyTypeIdentifiers() -> Option<NonNull<CFArray>> {
    Some(crate::coregraphics::owned(super::format::identifiers(&super::format::READABLE)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCreateWithDataProvider(
    provider: &CGDataProvider,
    options: Option<&CFDictionary>,
) -> Option<NonNull<CGImageSource>> {
    let bytes = crate::coregraphics::data::provider_imp(provider).bytes().unwrap_or_else(|| Arc::from(&[][..]));
    Some(crate::coregraphics::owned(make(bytes, true, options)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCreateWithData(
    data: &CFData,
    options: Option<&CFDictionary>,
) -> Option<NonNull<CGImageSource>> {
    // SAFETY: a CFData is an NSData here.
    let object = unsafe { &*(data as *const CFData).cast::<AnyObject>() };
    let bytes = crate::image_rep::data_bytes(object).unwrap_or_else(|| Arc::from(&[][..]));
    Some(crate::coregraphics::owned(make(bytes, true, options)))
}

/// A source of the file a URL names; none if it can't be read.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCreateWithURL(
    url: &CFURL,
    options: Option<&CFDictionary>,
) -> Option<NonNull<CGImageSource>> {
    let bytes = crate::image_rep::read_file(&crate::coregraphics::data::url_path(url)?)?;
    Some(crate::coregraphics::owned(make(bytes, true, options)))
}

/// The file's type identifier, which the source keeps alive (a constant).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceGetType(isrc: &CGImageSource) -> Option<NonNull<CFString>> {
    let constant = source_imp(isrc).kind()?.constant();
    // SAFETY: an ObjectRef is a pointer to an immortal object, laid out as
    // one.
    NonNull::new(unsafe { *(constant as *const sidestep_runtime::ObjectRef).cast::<*mut CFString>() })
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceGetCount(isrc: &CGImageSource) -> usize {
    source_imp(isrc).count()
}

/// The file's properties: its size and its format's own dictionary.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCopyProperties(
    isrc: &CGImageSource,
    _options: Option<&CFDictionary>,
) -> Option<NonNull<CFDictionary>> {
    let source = source_imp(isrc);
    let (info, len, complete) = {
        let state = source.state();
        (state.info.clone()?, state.data.len(), state.complete)
    };
    let mut dict = Dict::new();
    if complete {
        dict.set("FileSize", P::Long(len as i64));
    }
    for (k, v) in &info.file.0 {
        dict.set(k, v.clone());
    }
    Some(crate::coregraphics::owned(dict.to_object()))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCopyPropertiesAtIndex(
    isrc: &CGImageSource,
    index: usize,
    _options: Option<&CFDictionary>,
) -> Option<NonNull<CFDictionary>> {
    let info = source_imp(isrc).info()?;
    if index >= info.count {
        return None;
    }
    let props = info.images.get(index).map_or_else(Dict::new, |i| i.props.clone());
    Some(crate::coregraphics::owned(props.to_object()))
}

/// Metadata trees (XMP and the EXIF, IPTC and TIFF tags as tags) aren't
/// read: none, as for a file without metadata.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCopyMetadataAtIndex(
    _isrc: &CGImageSource,
    _index: usize,
    _options: Option<&CFDictionary>,
) -> Option<NonNull<c_void>> {
    None
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCreateImageAtIndex(
    isrc: &CGImageSource,
    index: usize,
    options: Option<&CFDictionary>,
) -> Option<NonNull<CGImage>> {
    source_imp(isrc).image_at(index, options).map(crate::coregraphics::owned)
}

/// Stop keeping the image made at `index`: the next one asked for is made
/// again once nothing else keeps it.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceRemoveCacheAtIndex(isrc: &CGImageSource, index: usize) {
    if let Some(slot) = source_imp(isrc).state().images.get_mut(index) {
        slot.0 = None;
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCreateThumbnailAtIndex(
    isrc: &CGImageSource,
    index: usize,
    options: Option<&CFDictionary>,
) -> Option<NonNull<CGImage>> {
    source_imp(isrc).thumbnail_at(index, options).map(crate::coregraphics::owned)
}

/// A source whose data arrives a piece at a time (`CGImageSourceUpdateData`).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCreateIncremental(
    options: Option<&CFDictionary>,
) -> Option<NonNull<CGImageSource>> {
    Some(crate::coregraphics::owned(make(Arc::from(&[][..]), false, options)))
}

fn update(isrc: &CGImageSource, bytes: Arc<[u8]>, last: bool) {
    let source = source_imp(isrc);
    let mut state = source.state();
    // All the data so far, each time; the last call's is the file.
    state.data = bytes;
    state.complete = last;
    refresh(&mut state);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceUpdateData(isrc: &CGImageSource, data: &CFData, r#final: bool) {
    // SAFETY: a CFData is an NSData here.
    let object = unsafe { &*(data as *const CFData).cast::<AnyObject>() };
    update(isrc, crate::image_rep::data_bytes(object).unwrap_or_else(|| Arc::from(&[][..])), r#final);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceUpdateDataProvider(
    isrc: &CGImageSource,
    provider: &CGDataProvider,
    r#final: bool,
) {
    let bytes = crate::coregraphics::data::provider_imp(provider).bytes().unwrap_or_else(|| Arc::from(&[][..]));
    update(isrc, bytes, r#final);
}

/// Complete once all the data is there and is a file of a type read here;
/// incomplete while more is coming; invalid data otherwise, as ImageIO
/// reports (measured).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceGetStatus(isrc: &CGImageSource) -> CGImageSourceStatus {
    let state = source_imp(isrc).state();
    match (&state.info, state.complete) {
        (None, _) => CGImageSourceStatus::StatusInvalidData,
        (Some(_), false) => CGImageSourceStatus::StatusIncomplete,
        (Some(_), true) => CGImageSourceStatus::StatusComplete,
    }
}

/// An image's status: of a file of no type read here, an unknown type (or
/// an unexpected end, for no data at all); past the last image, invalid
/// data; else incomplete or complete as the data is.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceGetStatusAtIndex(isrc: &CGImageSource, index: usize) -> CGImageSourceStatus {
    let state = source_imp(isrc).state();
    match &state.info {
        None if state.data.is_empty() => CGImageSourceStatus::StatusUnexpectedEOF,
        None => CGImageSourceStatus::StatusUnknownType,
        Some(info) if index >= info.count.max(1) => CGImageSourceStatus::StatusInvalidData,
        Some(_) if !state.complete => CGImageSourceStatus::StatusIncomplete,
        Some(_) => CGImageSourceStatus::StatusComplete,
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceGetPrimaryImageIndex(_isrc: &CGImageSource) -> usize {
    0
}

/// No file read here carries auxiliary images (depth, mattes, gain maps).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceCopyAuxiliaryDataInfoAtIndex(
    _isrc: &CGImageSource,
    _index: usize,
    _auxiliary_image_data_type: &CFString,
) -> Option<NonNull<CFDictionary>> {
    None
}

/// Read only the types named from now on (the process's sources). Always
/// succeeds (`noErr`); identifiers of types not read here are ignored.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageSourceSetAllowableTypes(allowable_types: &CFArray) -> i32 {
    // SAFETY: a CFArray is an NSArray here.
    let array = unsafe { &*(allowable_types as *const CFArray).cast::<objc2_foundation::NSArray<AnyObject>>() };
    let kinds = array
        .iter()
        .filter_map(|t| t.downcast::<NSString>().ok())
        .filter_map(|t| Kind::of_uti(&t.to_string()))
        .collect();
    super::format::set_allowed(kinds);
    0
}

#[cfg(test)]
mod tests {
    use super::fitted;

    #[test]
    fn thumbnails_are_sized_as_imageio_sizes_them() {
        // Measured on macOS: (image, size) and the thumbnail from a PNG and
        // from a JPEG.
        let cases = [
            ((48, 29), 16, (16, 9), (16, 9)),
            ((30, 20), 4, (4, 3), (3, 2)),
            ((1023, 767), 256, (256, 192), (255, 191)),
            ((129, 64), 65, (65, 32), (64, 32)),
            ((400, 200), 69, (69, 34), (69, 34)),
        ];
        for ((w, h), max, png, jpeg) in cases {
            assert_eq!(fitted(w, h, max, false), Some(png), "{w} × {h} in {max}, PNG");
            assert_eq!(fitted(w, h, max, true), Some(jpeg), "{w} × {h} in {max}, JPEG");
        }
        assert_eq!(fitted(1000, 3, 100, false), Some((100, 1)));
        assert_eq!(fitted(48, 29, 48, false), None, "it fits");
        assert_eq!(fitted(48, 29, 0, false), None);
    }
}
