//! `CGImageDestination`: writing images to image files, with the `image`
//! crate's encoders.
//!
//! A destination takes the images added to it (their pixels, premultiplied
//! RGBA, worked out when each is added) and writes the file when it's
//! finalized, as ImageIO does (measured): PNG, JPEG, TIFF and BMP files
//! hold the first image; a GIF holds them all, an animation when there is
//! more than one, each shown for its `{GIF}` delay (a tenth of a second
//! without one) and played as many times as the file's `{GIF}` loop count
//! says (once without one). Finalizing fails, writing nothing, when no
//! image was added or more were than the destination was made for, and a
//! second time; a destination of a type not written here isn't made.
//!
//! The properties written: `kCGImageDestinationLossyCompressionQuality`
//! (JPEG, 0.9 unless given), `kCGImagePropertyDPIWidth` and
//! `…DPIHeight` (a PNG's `pHYs`, a JPEG's JFIF density),
//! `kCGImageDestinationImageMaxPixelSize` (images larger are made smaller,
//! as thumbnails are) and the GIF delays and loop count. Images with
//! alpha are written with it where the type has it; a JPEG takes the
//! colors as drawn over black. Not written: EXIF, TIFF and other metadata,
//! orientation, color profiles, several pages of a TIFF, animated PNGs.

use std::io::Cursor;
use std::ptr::NonNull;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::{CFArray, CFDictionary, CFMutableData, CFString, CFTypeID, CFURL};
use objc2_core_graphics::{CGDataConsumer, CGImage, CGImageAlphaInfo};
use objc2_foundation::NSString;
use objc2_image_io::{
    CGImageDestination, CGImageSource, kCGImageDestinationImageMaxPixelSize,
    kCGImageDestinationLossyCompressionQuality, kCGImagePropertyDPIHeight, kCGImagePropertyDPIWidth,
    kCGImagePropertyGIFDelayTime, kCGImagePropertyGIFDictionary, kCGImagePropertyGIFLoopCount,
    kCGImagePropertyGIFUnclampedDelayTime,
};

use super::format::{Kind, WRITABLE};
use super::plist::read::{self, Options};
use crate::coregraphics::data::CGDataConsumerImpl;
use crate::coregraphics::image::{CGImageImpl, image_imp};

/// Where the file goes.
enum Sink {
    /// An `NSMutableData`, appended to.
    Data(Retained<AnyObject>),
    /// A file, written whole.
    Path(String),
    Consumer(Retained<CGDataConsumerImpl>),
}

/// An image added: its pixels and what was said of it.
struct Added {
    width: u32,
    height: u32,
    /// Premultiplied RGBA.
    rgba: Vec<u8>,
    alpha: bool,
    /// Seconds (a GIF frame's delay).
    delay: Option<f64>,
    quality: Option<f64>,
    dpi: Option<(f64, f64)>,
}

struct State {
    images: Vec<Added>,
    /// The file's loop count (`{GIF}`), from `CGImageDestinationSetProperties`.
    loops: Option<u32>,
    /// Quality and density set for the file, for images that don't say.
    quality: Option<f64>,
    dpi: Option<(f64, f64)>,
    finalized: bool,
}

pub(crate) struct DestinationIvars {
    sink: Sink,
    kind: Kind,
    count: usize,
    state: Mutex<State>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the state is behind
    // a mutex.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGImageDestination"]
    #[ivars = DestinationIvars]
    pub(crate) struct CGImageDestinationImpl;

    impl CGImageDestinationImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let rest = format!("'{}'", self.ivars().kind.uti());
            crate::coregraphics::description("CGImageDestination", self, &rest)
        }
    }

    unsafe impl NSObjectProtocol for CGImageDestinationImpl {}
);

fn destination_imp(d: &CGImageDestination) -> &CGImageDestinationImpl {
    // SAFETY: every CGImageDestination is a CGImageDestinationImpl.
    unsafe { &*(d as *const CGImageDestination).cast::<CGImageDestinationImpl>() }
}

/// The type a type identifier names, if it's one written here.
fn writable(t: &CFString) -> Option<Kind> {
    let t = read::key(t).to_string();
    WRITABLE.into_iter().find(|k| k.uti() == t)
}

fn make(sink: Sink, kind: Kind, count: usize, options: Option<&CFDictionary>) -> NonNull<CGImageDestination> {
    let o = read::dict(options);
    let (quality, dpi) = qualities(o);
    let state = State { images: Vec::new(), loops: None, quality, dpi, finalized: false };
    let this =
        CGImageDestinationImpl::alloc().set_ivars(DestinationIvars { sink, kind, count, state: Mutex::new(state) });
    // SAFETY: NSObject's designated initializer.
    let this: Retained<CGImageDestinationImpl> = unsafe { msg_send![super(this), init] };
    crate::coregraphics::owned(this)
}

/// The quality and density a dictionary of properties or options sets.
fn qualities(o: Option<&Options>) -> (Option<f64>, Option<(f64, f64)>) {
    // SAFETY: the keys are constants this module exports.
    unsafe {
        let quality = read::number(o, kCGImageDestinationLossyCompressionQuality);
        let x = read::number(o, kCGImagePropertyDPIWidth);
        let y = read::number(o, kCGImagePropertyDPIHeight);
        let dpi = match (x, y) {
            (Some(x), Some(y)) => Some((x, y)),
            (Some(d), None) | (None, Some(d)) => Some((d, d)),
            _ => None,
        };
        (quality, dpi)
    }
}

/// A `{GIF}` dictionary's delay, clamped or not.
fn gif_delay(o: Option<&Options>) -> Option<f64> {
    // SAFETY: the keys are constants this module exports.
    unsafe {
        let gif = read::sub(o, kCGImagePropertyGIFDictionary)?;
        read::number(Some(&gif), kCGImagePropertyGIFUnclampedDelayTime)
            .filter(|d| *d > 0.0)
            .or_else(|| read::number(Some(&gif), kCGImagePropertyGIFDelayTime))
    }
}

impl CGImageDestinationImpl {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.ivars().state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn add(&self, image: &CGImageImpl, properties: Option<&Options>, from: Option<&Options>) {
        let Some(rgba) = image.premultiplied() else { return };
        let (w, h) = (image.layout().width as u32, image.layout().height as u32);
        let alpha = !matches!(
            CGImageAlphaInfo(image.layout().info & crate::coregraphics::info::ALPHA_MASK),
            CGImageAlphaInfo::None | CGImageAlphaInfo::NoneSkipFirst | CGImageAlphaInfo::NoneSkipLast
        ) || image.is_mask();
        let (quality, dpi) = qualities(properties);
        let delay = gif_delay(properties).or_else(|| gif_delay(from));
        let mut added = Added { width: w, height: h, rgba: rgba.to_vec(), alpha, delay, quality, dpi };
        // SAFETY: the key is a constant this module exports.
        let max = unsafe { read::number(properties, kCGImageDestinationImageMaxPixelSize) };
        if let Some((tw, th)) =
            max.filter(|m| *m >= 1.0).and_then(|m| super::source::fitted(w, h, m.floor() as u32, false))
            && let Some(buffer) = image::RgbaImage::from_raw(w, h, std::mem::take(&mut added.rgba))
        {
            added.rgba = image::imageops::thumbnail(&buffer, tw, th).into_raw();
            (added.width, added.height) = (tw, th);
        }
        self.state().images.push(added);
    }

    /// The file's bytes, or `None` if the images can't be written.
    fn encode(&self, state: &State) -> Option<Vec<u8>> {
        let kind = self.ivars().kind;
        if kind == Kind::Gif {
            return encode_gif(&state.images, state.loops);
        }
        let first = state.images.first()?;
        let quality = first.quality.or(state.quality);
        let dpi = first.dpi.or(state.dpi);
        let straight = straight(&first.rgba);
        let image = image::RgbaImage::from_raw(first.width, first.height, straight)?;
        let image = if first.alpha {
            image::DynamicImage::ImageRgba8(image)
        } else {
            image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(image).into_rgb8())
        };
        let mut out = Cursor::new(Vec::new());
        match kind {
            Kind::Jpeg => {
                // The colors as drawn over black: premultiplied, alpha dropped.
                let rgb: Vec<u8> = first.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
                let rgb = image::RgbImage::from_raw(first.width, first.height, rgb)?;
                let q = (quality.unwrap_or(0.9).clamp(0.0, 1.0) * 100.0).round().max(1.0) as u8;
                let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
                if let Some((x, y)) = dpi {
                    encoder.set_pixel_density(image::codecs::jpeg::PixelDensity {
                        density: (x.round().clamp(1.0, 65535.0) as u16, y.round().clamp(1.0, 65535.0) as u16),
                        unit: image::codecs::jpeg::PixelDensityUnit::Inches,
                    });
                }
                rgb.write_with_encoder(encoder).ok()?;
            }
            Kind::Png => {
                image.write_to(&mut out, image::ImageFormat::Png).ok()?;
                if let Some(dpi) = dpi {
                    return Some(with_phys(out.into_inner(), dpi));
                }
            }
            Kind::Tiff => image.write_to(&mut out, image::ImageFormat::Tiff).ok()?,
            Kind::Bmp => image.write_to(&mut out, image::ImageFormat::Bmp).ok()?,
            _ => return None,
        }
        Some(out.into_inner())
    }

    fn finalize(&self) -> bool {
        let mut state = self.state();
        if state.finalized {
            return false;
        }
        state.finalized = true;
        if state.images.is_empty() || state.images.len() > self.ivars().count {
            return false;
        }
        let Some(bytes) = self.encode(&state) else { return false };
        match &self.ivars().sink {
            Sink::Data(d) => {
                // SAFETY: an NSMutableData answers -appendBytes:length:.
                let _: () = unsafe {
                    msg_send![&**d, appendBytes: bytes.as_ptr().cast::<std::ffi::c_void>(), length: bytes.len()]
                };
                true
            }
            Sink::Path(p) => std::fs::write(p, &bytes).is_ok(),
            Sink::Consumer(c) => c.put(&bytes) == bytes.len(),
        }
    }
}

/// Premultiplied RGBA as straight.
fn straight(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let a = u32::from(p[3]);
            if a == 0 || a == 255 {
                return *p;
            }
            let u = |c: u8| ((u32::from(c) * 255 + a / 2) / a).min(255) as u8;
            [u(p[0]), u(p[1]), u(p[2]), p[3]]
        })
        .collect()
}

/// A GIF of `images`, one frame each.
fn encode_gif(images: &[Added], loops: Option<u32>) -> Option<Vec<u8>> {
    use image::codecs::gif::{GifEncoder, Repeat};
    let mut out = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut out);
        // Plays `loops` times: the file says how many times to play again
        // (`crate::codec::gif_frames` reads it back the same way).
        match loops {
            Some(0) => encoder.set_repeat(Repeat::Infinite).ok()?,
            Some(n) if n > 1 => encoder.set_repeat(Repeat::Finite((n - 1).min(u32::from(u16::MAX)) as u16)).ok()?,
            _ => {}
        }
        for image in images {
            let buffer = image::RgbaImage::from_raw(image.width, image.height, straight(&image.rgba))?;
            let ms = (image.delay.unwrap_or(0.1) * 1000.0).round().max(0.0) as u32;
            let frame = image::Frame::from_parts(buffer, 0, 0, image::Delay::from_numer_denom_ms(ms, 1));
            encoder.encode_frame(frame).ok()?;
        }
    }
    Some(out)
}

/// `png` with a `pHYs` chunk giving `dpi`, right after its header.
fn with_phys(mut png: Vec<u8>, dpi: (f64, f64)) -> Vec<u8> {
    let per_meter = |d: f64| (d / 0.0254).round().clamp(1.0, f64::from(u32::MAX)) as u32;
    let mut body = b"pHYs".to_vec();
    body.extend_from_slice(&per_meter(dpi.0).to_be_bytes());
    body.extend_from_slice(&per_meter(dpi.1).to_be_bytes());
    body.push(1);
    let mut chunk = 9u32.to_be_bytes().to_vec();
    chunk.extend_from_slice(&body);
    chunk.extend_from_slice(&crc32(&body).to_be_bytes());
    // After the signature (8 bytes) and the header chunk (25).
    if png.len() >= 33 {
        png.splice(33..33, chunk);
    }
    png
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

// The functions.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageDestinationGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_IMAGE_DESTINATION
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageDestinationCopyTypeIdentifiers() -> Option<NonNull<CFArray>> {
    Some(crate::coregraphics::owned(super::format::identifiers(&WRITABLE)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageDestinationCreateWithDataConsumer(
    consumer: &CGDataConsumer,
    r#type: &CFString,
    count: usize,
    options: Option<&CFDictionary>,
) -> Option<NonNull<CGImageDestination>> {
    let kind = writable(r#type)?;
    let consumer = crate::coregraphics::data::consumer_imp(consumer).retain();
    Some(make(Sink::Consumer(consumer), kind, count, options))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageDestinationCreateWithData(
    data: &CFMutableData,
    r#type: &CFString,
    count: usize,
    options: Option<&CFDictionary>,
) -> Option<NonNull<CGImageDestination>> {
    let kind = writable(r#type)?;
    // SAFETY: a CFMutableData is an NSMutableData here.
    let data = unsafe { &*(data as *const CFMutableData).cast::<AnyObject>() }.retain();
    Some(make(Sink::Data(data), kind, count, options))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageDestinationCreateWithURL(
    url: &CFURL,
    r#type: &CFString,
    count: usize,
    options: Option<&CFDictionary>,
) -> Option<NonNull<CGImageDestination>> {
    let kind = writable(r#type)?;
    let path = crate::coregraphics::data::url_path(url)?;
    Some(make(Sink::Path(path), kind, count, options))
}

/// File-wide properties: the GIF loop count, and the quality and density
/// for images that don't give their own.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageDestinationSetProperties(idst: &CGImageDestination, properties: Option<&CFDictionary>) {
    let d = destination_imp(idst);
    let o = read::dict(properties);
    let (quality, dpi) = qualities(o);
    // SAFETY: the keys are constants this module exports.
    let loops = unsafe {
        read::sub(o, kCGImagePropertyGIFDictionary)
            .and_then(|g| read::number(Some(&g), kCGImagePropertyGIFLoopCount))
            .map(|n| n.max(0.0) as u32)
    };
    let mut state = d.state();
    state.loops = loops.or(state.loops);
    state.quality = quality.or(state.quality);
    state.dpi = dpi.or(state.dpi);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageDestinationAddImage(
    idst: &CGImageDestination,
    image: &CGImage,
    properties: Option<&CFDictionary>,
) {
    destination_imp(idst).add(image_imp(image), read::dict(properties), None);
}

/// The image at `index` of a source, with its properties (a GIF frame's
/// delay) under those given.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageDestinationAddImageFromSource(
    idst: &CGImageDestination,
    isrc: &CGImageSource,
    index: usize,
    properties: Option<&CFDictionary>,
) {
    let source = super::source::source_imp(isrc);
    let Some(image) = source.image_at(index, None) else { return };
    let from = super::source::CGImageSourceCopyPropertiesAtIndex(isrc, index, None).map(|p| {
        // SAFETY: a +1 dictionary from the Copy function.
        unsafe { Retained::from_raw(p.as_ptr().cast::<Options>()) }
    });
    let from = from.flatten();
    destination_imp(idst).add(&image, read::dict(properties), from.as_deref());
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageDestinationFinalize(idst: &CGImageDestination) -> bool {
    destination_imp(idst).finalize()
}
