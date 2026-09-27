//! `CGImage`: pixels in a layout (bits per component and pixel, row
//! length, color space, alpha and byte order, a decode array), from a data
//! provider; image masks (`CGImageMaskCreate`), parts of images, images
//! masked by others or by color ranges.
//!
//! An image is immutable. Drawing takes it as premultiplied RGBA, 8 bits a
//! sample ([`CGImageImpl::pixels`]): worked out from its provider's bytes
//! the first time it's drawn, then kept and shared with every context
//! drawing it, on any thread, as an image representation's snapshot is
//! (`raster::images`: the same cache, keyed by the image). The common
//! layouts (8-bit RGB and gray, alpha first or last, premultiplied,
//! straight or skipped, either byte order, in a space whose samples are
//! drawn as they are) convert row by row; the others sample by sample.
//! Layouts read: 1, 2, 4, 8, 16 and 32-bit integer and 16 and 32-bit
//! floating-point samples (and 5-bit RGB in 16-bit pixels), gray, RGB,
//! CMYK, Lab, XYZ and indexed colors, alpha first or last, premultiplied or
//! not or skipped, in either byte order. A decode array maps each color
//! sample's range from its two values to 0 to 1, and leaves alpha alone, as
//! CoreGraphics does (measured on macOS). An image mask is drawn in the
//! fill color where its samples are low, as CoreGraphics paints a stencil.
//!
//! Where pixels are worked out: an image of an encoded file keeps the file
//! and is decoded where it's drawn (the render thread, for windows), as
//! `NSImage`'s are; a part of an image (`CGImageCreateWithImageInRect`) is
//! cropped from its image's cached pixels there too, so a sprite sheet is
//! decoded once however many parts of it are drawn. Pixels asked for here
//! (masks, AppKit's bridges) are worked out once and kept.

use std::ffi::c_float;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::{CFString, CFTypeID, CGFloat, CGRect};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGImagePixelFormatInfo,
};
use objc2_foundation::NSString;

use super::color::{CGColorSpaceImpl, Model, space_imp};
use super::data::{CGDataProviderImpl, provider_imp};
use super::info::{
    ALPHA_MASK, FLOAT, FORMAT_MASK, ORDER_16_BIG, ORDER_16_LITTLE, ORDER_32_BIG, ORDER_32_LITTLE, ORDER_MASK,
};
use crate::context::ContextState;
use crate::protocol::{ClipPath, Op, Quality, Rect};
use crate::raster::images::{ImageData, Pixels};

/// The most samples a pixel has: CMYK and alpha.
const MOST_SAMPLES: usize = 5;

/// How an image's samples are laid out, and what they are.
#[derive(Clone)]
pub(crate) struct Layout {
    pub width: usize,
    pub height: usize,
    pub bpc: usize,
    pub bpp: usize,
    pub bpr: usize,
    /// `None` for an image mask or an alpha-only image.
    pub space: Option<Retained<CGColorSpaceImpl>>,
    pub info: u32,
    pub decode: Option<Vec<f64>>,
}

impl Layout {
    fn alpha(&self) -> CGImageAlphaInfo {
        CGImageAlphaInfo(self.info & ALPHA_MASK)
    }

    fn colors(&self) -> usize {
        self.space.as_ref().map_or(0, |s| s.info().components)
    }

    /// The samples a pixel has: colors, and alpha or padding (a mask's or
    /// an alpha-only image's one sample).
    fn samples(&self) -> usize {
        if self.space.is_none() {
            return 1;
        }
        let extra = !matches!(self.alpha(), CGImageAlphaInfo::None);
        self.colors() + usize::from(extra)
    }

    /// Which sample is alpha (not padding), if one is.
    fn alpha_sample(&self) -> Option<usize> {
        match self.alpha() {
            CGImageAlphaInfo::PremultipliedLast | CGImageAlphaInfo::Last => Some(self.colors()),
            CGImageAlphaInfo::PremultipliedFirst | CGImageAlphaInfo::First => Some(0),
            CGImageAlphaInfo::Only if self.space.is_none() => Some(0),
            _ => None,
        }
    }

    fn float(&self) -> bool {
        self.info & FLOAT != 0
    }

    fn little(&self) -> bool {
        matches!(self.info & ORDER_MASK, ORDER_16_LITTLE | ORDER_32_LITTLE)
    }

    /// The bytes the pixels take: all rows but the last whole, the last
    /// only as far as its pixels go.
    fn len(&self) -> Option<usize> {
        let row = self.width.checked_mul(self.bpp)?.div_ceil(8);
        self.bpr.checked_mul(self.height.checked_sub(1)?)?.checked_add(row)
    }

    /// Whether the layout is one images come in: supported samples, room
    /// for them in a pixel and a pixel in a row, and a size whose bytes and
    /// drawn pixels can be counted (whatever the provider says of its own
    /// length).
    fn valid(&self) -> bool {
        let depth_ok = match (self.bpc, self.float()) {
            (1 | 2 | 4 | 8 | 16 | 32, false) => true,
            (16 | 32, true) => true,
            // RGB in 16-bit words, a padding bit first or last.
            (5, false) => {
                self.bpp == 16
                    && self.colors() == 3
                    && matches!(self.alpha(), CGImageAlphaInfo::NoneSkipFirst | CGImageAlphaInfo::NoneSkipLast)
            }
            _ => false,
        };
        let pixel_ok = self.bpc == 5
            || (self.bpp >= self.bpc * self.samples() && (self.bpc >= 8 || self.bpp == self.bpc * self.samples()));
        // Colors without alpha or padding take the whole pixel: CoreGraphics
        // makes no RGB image of 32 bits a pixel without them.
        let exact = self.space.is_none()
            || self.alpha() != CGImageAlphaInfo::None
            || self.bpc == 5
            || self.bpp == self.bpc * self.samples();
        let space_ok = match &self.space {
            Some(s) => !matches!(s.info().model, Model::Pattern) && self.alpha() != CGImageAlphaInfo::Only,
            None => matches!(self.alpha(), CGImageAlphaInfo::None | CGImageAlphaInfo::Only),
        };
        let drawn = self.width.checked_mul(self.height).and_then(|n| n.checked_mul(4));
        self.width > 0
            && self.height > 0
            && depth_ok
            && pixel_ok
            && exact
            && space_ok
            && self.width.checked_mul(self.bpp).is_some_and(|b| b.div_ceil(8) <= self.bpr)
            && self.len().is_some_and(|n| n <= isize::MAX as usize)
            && drawn.is_some_and(|n| n <= isize::MAX as usize)
    }
}

/// What else shapes an image's pixels.
#[derive(Clone)]
enum Derived {
    None,
    /// Part of another image: its left and top pixel there.
    Part(Retained<CGImageImpl>, usize, usize),
    /// Another image's pixels, masked by a mask image's coverage.
    Masked(Retained<CGImageImpl>, Retained<CGImageImpl>),
    /// Another image's pixels, clear where its samples are in these ranges
    /// (a minimum and maximum a component).
    ColorMasked(Retained<CGImageImpl>, Vec<f64>),
}

pub(crate) struct ImageIvars {
    layout: Layout,
    /// Where the bytes come from; for a part, made from its image's when
    /// asked for (`part_provider`).
    provider: Option<Retained<CGDataProviderImpl>>,
    interpolate: bool,
    intent: CGColorRenderingIntent,
    mask: bool,
    derived: Derived,
    /// The file type (`public.png`, …) of an image of a file.
    file_type: Option<&'static sidestep_runtime::ObjectRef>,
    key: u64,
    /// The pixels as drawing takes them, once worked out.
    pixels: OnceLock<Option<Arc<ImageData>>>,
    /// The pixels as premultiplied RGBA here, once asked for (an image of
    /// a file, or a part, has none until then).
    rgba: OnceLock<Option<Arc<[u8]>>>,
    /// What `CGContextClipToMask` clips to, once worked out.
    clip: OnceLock<Option<Arc<ImageData>>>,
    /// A part's provider: its rows of its image's bytes.
    part_provider: OnceLock<Option<Retained<CGDataProviderImpl>>>,
    /// Drawn in a window, so the render thread may have it cached.
    recorded: AtomicBool,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; an image is
    // immutable, and what's worked out of it is kept behind OnceLocks.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGImage"]
    #[ivars = ImageIvars]
    pub(crate) struct CGImageImpl;

    impl CGImageImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let l = &self.ivars().layout;
            let rest = format!(
                "width = {}, height = {}, bpc = {}, bpp = {}, row bytes = {}, is mask? {}",
                l.width,
                l.height,
                l.bpc,
                l.bpp,
                l.bpr,
                if self.ivars().mask { "Yes" } else { "No" }
            );
            super::description("CGImage", self, &rest)
        }
    }

    unsafe impl NSObjectProtocol for CGImageImpl {}
);

impl Drop for CGImageImpl {
    fn drop(&mut self) {
        let recorded = self.ivars().recorded.load(Ordering::Relaxed);
        if self.ivars().pixels.get().is_some_and(Option::is_some) {
            crate::image_rep::forget(self.ivars().key, recorded);
        }
        if let Some(Some(clip)) = self.ivars().clip.get() {
            crate::image_rep::forget(clip.key, recorded);
        }
    }
}

pub(crate) fn image_imp(i: &CGImage) -> &CGImageImpl {
    // SAFETY: every CGImage is a CGImageImpl.
    unsafe { &*(i as *const CGImage).cast::<CGImageImpl>() }
}

impl CGImageImpl {
    pub(crate) fn layout(&self) -> &Layout {
        &self.ivars().layout
    }

    pub(crate) fn is_mask(&self) -> bool {
        self.ivars().mask
    }

    pub(crate) fn as_cg(&self) -> &CGImage {
        // SAFETY: CGImageImpl is what CGImage names.
        unsafe { &*(self as *const Self).cast::<CGImage>() }
    }

    /// The bytes of an image made of its provider's alone (not a mask, a
    /// part or a masked image), if the provider has them all.
    pub(crate) fn source_bytes(&self) -> Option<Arc<[u8]>> {
        if self.is_mask() || !matches!(self.ivars().derived, Derived::None) {
            return None;
        }
        let bytes = self.ivars().provider.as_ref()?.bytes()?;
        (bytes.len() >= self.layout().len()?).then_some(bytes)
    }

    /// The encoded file an image of a file keeps, and whether it's turned
    /// upright.
    pub(crate) fn file(&self) -> Option<(Arc<[u8]>, bool)> {
        match &self.ivars().pixels.get()?.as_ref()?.pixels {
            Pixels::Encoded(file, upright) if matches!(self.ivars().derived, Derived::None) => {
                Some((file.clone(), *upright))
            }
            _ => None,
        }
    }

    /// The pixels as drawing takes them: premultiplied RGBA, rows
    /// `width × 4` bytes apart (an image mask's coverage as black); a
    /// part's cropped from its image's where it's drawn.
    pub(crate) fn pixels(&self) -> Option<Arc<ImageData>> {
        self.ivars()
            .pixels
            .get_or_init(|| {
                let l = &self.ivars().layout;
                let pixels = match &self.ivars().derived {
                    Derived::Part(parent, x, y) => {
                        Pixels::Part(parent.pixels()?, u32::try_from(*x).ok()?, u32::try_from(*y).ok()?)
                    }
                    _ => Pixels::Rgba(self.premultiplied()?),
                };
                Some(Arc::new(ImageData {
                    key: self.ivars().key,
                    generation: 0,
                    width: u32::try_from(l.width).ok()?,
                    height: u32::try_from(l.height).ok()?,
                    pixels,
                }))
            })
            .clone()
    }

    fn rgba(&self) -> Option<Vec<u8>> {
        let l = &self.ivars().layout;
        match &self.ivars().derived {
            Derived::None => {
                let bytes = self.ivars().provider.as_ref()?.bytes()?;
                if bytes.len() < l.len()? {
                    return None;
                }
                Some(if self.ivars().mask { mask_rgba(l, &bytes) } else { decode(l, &bytes) })
            }
            Derived::Part(parent, x, y) => {
                let src = parent.premultiplied()?;
                let pw = parent.layout().width;
                let mut out = Vec::with_capacity(l.width * l.height * 4);
                for row in 0..l.height {
                    let at = ((y + row) * pw + x) * 4;
                    out.extend_from_slice(src.get(at..at + l.width * 4)?);
                }
                Some(out)
            }
            Derived::Masked(image, mask) => {
                let mut out = image.premultiplied()?.to_vec();
                let (mw, mh) = (mask.layout().width, mask.layout().height);
                let coverage = mask.coverage()?;
                for y in 0..l.height {
                    for x in 0..l.width {
                        // The mask stretched over the image, nearest sample.
                        let (mx, my) = (x * mw / l.width, y * mh / l.height);
                        let c = u32::from(coverage[my * mw + mx]);
                        let p = &mut out[(y * l.width + x) * 4..][..4];
                        for v in p.iter_mut() {
                            *v = ((u32::from(*v) * c + 127) / 255) as u8;
                        }
                    }
                }
                Some(out)
            }
            Derived::ColorMasked(image, ranges) => {
                let il = image.layout();
                let bytes = image.ivars().provider.as_ref()?.bytes()?;
                let mut out = image.premultiplied()?.to_vec();
                let max = if il.float() { 1.0 } else { ((1u64 << il.bpc.min(32)) - 1) as f64 };
                for y in 0..il.height {
                    for x in 0..il.width {
                        let raw = read_pixel(il, &bytes, x, y);
                        let inside = (0..il.colors()).all(|i| {
                            let v = raw[i] * max;
                            ranges.get(2 * i).is_some_and(|lo| v >= *lo)
                                && ranges.get(2 * i + 1).is_some_and(|hi| v <= *hi)
                        });
                        if inside {
                            out[(y * il.width + x) * 4..][..4].fill(0);
                        }
                    }
                }
                Some(out)
            }
        }
    }

    /// The pixels as premultiplied RGBA, rows `width × 4` bytes apart:
    /// worked out here the first time they're asked for (for an image of a
    /// file, or a part, which drawing works out elsewhere), then kept.
    pub(crate) fn premultiplied(&self) -> Option<Arc<[u8]>> {
        self.ivars()
            .rgba
            .get_or_init(|| match self.ivars().pixels.get().and_then(|p| p.as_deref()).map(|d| &d.pixels) {
                Some(Pixels::Rgba(rgba)) => Some(rgba.clone()),
                _ => self.rgba().map(Arc::from),
            })
            .clone()
    }

    /// How much each pixel lets through as a mask, as CoreGraphics takes
    /// images as masks (measured on macOS): an image mask's stencil (its
    /// low samples paint); an image with alpha, its alpha; a gray image,
    /// its levels; a color image without alpha, its gray.
    pub(crate) fn coverage(&self) -> Option<Arc<[u8]>> {
        let rgba = self.premultiplied()?;
        let l = self.layout();
        let px = rgba.as_chunks::<4>().0.iter();
        let gray = l.space.as_ref().is_some_and(|s| s.info().model == Model::Gray);
        Some(if self.is_mask() || l.alpha_sample().is_some() {
            px.map(|p| p[3]).collect()
        } else if gray {
            px.map(|p| p[0]).collect()
        } else {
            px.map(|p| crate::raster::pixels::gray8(*p)).collect()
        })
    }

    /// What `CGContextClipToMask` clips to: the coverage, as an image;
    /// worked out once.
    pub(crate) fn clip_pixels(&self) -> Option<Arc<ImageData>> {
        self.ivars()
            .clip
            .get_or_init(|| {
                let coverage = self.coverage()?;
                let l = self.layout();
                let rgba: Vec<u8> = coverage.iter().flat_map(|&c| [0, 0, 0, c]).collect();
                Some(Arc::new(ImageData {
                    key: crate::raster::images::next_key(),
                    generation: 0,
                    width: l.width as u32,
                    height: l.height as u32,
                    pixels: Pixels::Rgba(Arc::from(rgba)),
                }))
            })
            .clone()
    }

    /// The provider of a part: its rows of its image's bytes, in its
    /// image's layout (rows `bpr` apart, the last as long as its pixels),
    /// as CoreGraphics hands out a part's.
    fn part_provider(&self) -> Option<Retained<CGDataProviderImpl>> {
        let Derived::Part(parent, x, y) = &self.ivars().derived else { return None };
        self.ivars()
            .part_provider
            .get_or_init(|| {
                let pl = parent.layout();
                let bytes = parent.data_provider()?.bytes()?;
                let l = self.layout();
                let mut out = vec![0u8; l.len()?];
                let row_bits = l.width * l.bpp;
                for row in 0..l.height {
                    let src = (y + row) * pl.bpr;
                    let dst = row * l.bpr;
                    let first = x * pl.bpp;
                    if first % 8 == 0 {
                        let from = src + first / 8;
                        let n = row_bits.div_ceil(8);
                        out.get_mut(dst..dst + n)?.copy_from_slice(bytes.get(from..from + n)?);
                    } else {
                        // Packed samples not on a byte: bit by bit.
                        for bit in 0..row_bits {
                            let s = src * 8 + first + bit;
                            let on = bytes.get(s / 8)? >> (7 - s % 8) & 1;
                            let d = dst * 8 + bit;
                            *out.get_mut(d / 8)? |= on << (7 - d % 8);
                        }
                    }
                }
                Some(super::data::of_bytes(Arc::from(out)))
            })
            .clone()
    }

    /// The provider `CGImageGetDataProvider` hands out.
    fn data_provider(&self) -> Option<Retained<CGDataProviderImpl>> {
        match &self.ivars().derived {
            Derived::Part(..) => self.part_provider(),
            Derived::Masked(p, _) | Derived::ColorMasked(p, _) => {
                self.ivars().provider.clone().or_else(|| p.data_provider())
            }
            Derived::None => self.ivars().provider.clone(),
        }
    }
}

/// An IEEE half-precision float's value.
fn half(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((bits >> 10) & 0x1f);
    let frac = f64::from(bits & 0x3ff);
    sign * match exp {
        0 => frac * 2f64.powi(-24),
        31 if frac == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        e => (1.0 + frac / 1024.0) * 2f64.powi(e - 15),
    }
}

/// Pixel (x, y)'s samples, each 0 to 1 (colors decoded), in their order in
/// the pixel, after undoing a little-endian byte order of 8-bit samples.
fn read_pixel(l: &Layout, bytes: &[u8], x: usize, y: usize) -> [f64; MOST_SAMPLES] {
    let n = l.samples().min(MOST_SAMPLES);
    let order = l.info & ORDER_MASK;
    let at = y * l.bpr;
    let mut out = [0.0; MOST_SAMPLES];
    let byte = |i: usize| bytes.get(i).copied().unwrap_or(0);
    match l.bpc {
        5 => {
            let px = at + x * 2;
            let b = [byte(px), byte(px + 1)];
            let word = if l.little() { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) };
            let first = l.alpha() == CGImageAlphaInfo::NoneSkipFirst;
            let shift = if first { 10 } else { 11 };
            let c = |k: u16| f64::from((word >> (shift - 5 * k)) & 0x1f) / 31.0;
            out[..4].copy_from_slice(&if first { [0.0, c(0), c(1), c(2)] } else { [c(0), c(1), c(2), 0.0] });
        }
        8 => {
            let px = at + x * l.bpp / 8;
            let size = l.bpp / 8;
            let reversed = (order == ORDER_32_LITTLE && size == 4) || (order == ORDER_16_LITTLE && size == 2);
            for (i, v) in out.iter_mut().enumerate().take(n) {
                let k = if reversed { size - 1 - i } else { i };
                *v = f64::from(byte(px + k)) / 255.0;
            }
        }
        16 => {
            let px = at + x * l.bpp / 8;
            for (i, v) in out.iter_mut().enumerate().take(n) {
                let b = [byte(px + 2 * i), byte(px + 2 * i + 1)];
                let raw = if l.little() { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) };
                *v = if l.float() { half(raw) } else { f64::from(raw) / 65535.0 };
            }
        }
        32 => {
            let px = at + x * l.bpp / 8;
            for (i, v) in out.iter_mut().enumerate().take(n) {
                let b = [byte(px + 4 * i), byte(px + 4 * i + 1), byte(px + 4 * i + 2), byte(px + 4 * i + 3)];
                let raw = if l.little() { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) };
                *v = if l.float() { f64::from(f32::from_bits(raw)) } else { f64::from(raw) / f64::from(u32::MAX) };
            }
        }
        bpc => {
            // Packed samples, the most significant bits first.
            let max = ((1u32 << bpc) - 1) as f64;
            let first = x * l.bpp;
            for (i, v) in out.iter_mut().enumerate().take(n) {
                let bit = first + i * bpc;
                let shift = 8 - bpc - bit % 8;
                *v = f64::from((byte(at + bit / 8) >> shift) & ((1 << bpc) - 1) as u8) / max;
            }
        }
    }
    if let Some(decode) = &l.decode {
        let indexed = l.space.as_ref().is_some_and(|s| s.info().model == Model::Indexed);
        let alpha = if l.space.is_some() { l.alpha_sample() } else { None };
        for (i, v) in out.iter_mut().enumerate().take(n) {
            let (Some(lo), Some(hi)) = (decode.get(2 * i), decode.get(2 * i + 1)) else { continue };
            if Some(i) == alpha {
                continue;
            }
            *v = if indexed {
                // An index: the decode array spans the indices.
                lo + *v * (hi - lo)
            } else if hi == lo {
                0.0
            } else {
                // CoreGraphics maps the range from `lo` to `hi` onto 0 to 1.
                (*v - lo) / (hi - lo)
            };
        }
    }
    out
}

/// Premultiply 8-bit `c` by `a`.
fn times(c: u8, a: u8) -> u8 {
    ((u32::from(c) * u32::from(a) + 127) / 255) as u8
}

/// Row by row, for the common 8-bit layouts in a space whose samples are
/// drawn as they are: `false` if `l` isn't one.
fn decode_fast(l: &Layout, space: &CGColorSpaceImpl, bytes: &[u8], out: &mut [u8]) -> bool {
    let info = space.info();
    if l.bpc != 8 || l.float() || l.decode.is_some() || !space.samples_are_srgb() {
        return false;
    }
    let colors = info.components;
    let alpha = l.alpha();
    let size = l.bpp / 8;
    let order = l.info & ORDER_MASK;
    // Each sample's byte in the pixel, in logical order (alpha or padding
    // first or last), after a little-endian word's reversal.
    let reversed = (order == ORDER_32_LITTLE && size == 4) || (order == ORDER_16_LITTLE && size == 2);
    let byte = |i: usize| if reversed { size - 1 - i } else { i };
    let (first, extra) = match alpha {
        CGImageAlphaInfo::None => (false, false),
        CGImageAlphaInfo::PremultipliedFirst | CGImageAlphaInfo::First | CGImageAlphaInfo::NoneSkipFirst => {
            (true, true)
        }
        CGImageAlphaInfo::PremultipliedLast | CGImageAlphaInfo::Last | CGImageAlphaInfo::NoneSkipLast => (false, true),
        _ => return false,
    };
    if !matches!(colors, 1 | 3) || size != colors + usize::from(extra) {
        return false;
    }
    let c0 = usize::from(first);
    let (r, g, b) = if colors == 3 { (byte(c0), byte(c0 + 1), byte(c0 + 2)) } else { (byte(c0), byte(c0), byte(c0)) };
    let a = match alpha {
        CGImageAlphaInfo::PremultipliedFirst | CGImageAlphaInfo::First => Some(byte(0)),
        CGImageAlphaInfo::PremultipliedLast | CGImageAlphaInfo::Last => Some(byte(colors)),
        _ => None,
    };
    let straight = matches!(alpha, CGImageAlphaInfo::First | CGImageAlphaInfo::Last);
    for y in 0..l.height {
        let Some(row) = bytes.get(y * l.bpr..y * l.bpr + l.width * size) else { return true };
        let dst = &mut out[y * l.width * 4..][..l.width * 4];
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(row.chunks_exact(size)) {
            *d = match a {
                None => [s[r], s[g], s[b], 255],
                Some(ai) if straight => {
                    let a = s[ai];
                    [times(s[r], a), times(s[g], a), times(s[b], a), a]
                }
                Some(ai) => {
                    let a = s[ai];
                    [s[r].min(a), s[g].min(a), s[b].min(a), a]
                }
            };
        }
    }
    true
}

/// An image's pixels as premultiplied RGBA (an alpha-only image's in
/// black).
fn decode(l: &Layout, bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; l.width * l.height * 4];
    let Some(space) = &l.space else {
        for y in 0..l.height {
            for x in 0..l.width {
                let a = read_pixel(l, bytes, x, y)[0];
                out[(y * l.width + x) * 4 + 3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        return out;
    };
    if decode_fast(l, space, bytes, &mut out) {
        return out;
    }
    let colors = space.info().components.min(MOST_SAMPLES - 1);
    let indexed = space.info().model == Model::Indexed;
    let index_max = ((1u64 << l.bpc.min(16)) - 1) as f64;
    let alpha = l.alpha();
    for y in 0..l.height {
        for x in 0..l.width {
            let s = read_pixel(l, bytes, x, y);
            let (at, a, premultiplied) = match alpha {
                CGImageAlphaInfo::PremultipliedLast => (0, s[colors], true),
                CGImageAlphaInfo::PremultipliedFirst => (1, s[0], true),
                CGImageAlphaInfo::Last => (0, s[colors], false),
                CGImageAlphaInfo::First => (1, s[0], false),
                CGImageAlphaInfo::NoneSkipFirst => (1, 1.0, false),
                _ => (0, 1.0, false),
            };
            let a = a.clamp(0.0, 1.0);
            let mut c = [0.0; MOST_SAMPLES - 1];
            for (k, v) in c.iter_mut().enumerate().take(colors) {
                let raw = s[at + k];
                *v = if premultiplied && a > 0.0 { raw / a } else { raw };
            }
            if indexed && l.decode.is_none() {
                c[0] = (c[0] * index_max).round();
            }
            let rgb = space.samples_to_srgb(&c[..colors]);
            let q = |v: f64| (v.clamp(0.0, 1.0) * a * 255.0).round() as u8;
            out[(y * l.width + x) * 4..][..4].copy_from_slice(&[
                q(rgb[0]),
                q(rgb[1]),
                q(rgb[2]),
                (a * 255.0).round() as u8,
            ]);
        }
    }
    out
}

/// An image mask's pixels: black, where its samples are low (1 minus the
/// decoded sample is the coverage).
fn mask_rgba(l: &Layout, bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; l.width * l.height * 4];
    for y in 0..l.height {
        for x in 0..l.width {
            let s = read_pixel(l, bytes, x, y);
            let cover = (1.0 - s[0]).clamp(0.0, 1.0);
            out[(y * l.width + x) * 4 + 3] = (cover * 255.0).round() as u8;
        }
    }
    out
}

/// What an image is made of beyond its layout and provider.
struct Made {
    interpolate: bool,
    intent: CGColorRenderingIntent,
    mask: bool,
    derived: Derived,
    file_type: Option<&'static sidestep_runtime::ObjectRef>,
}

impl Made {
    /// An image of its provider's bytes, drawn with interpolation.
    fn plain() -> Made {
        Made {
            interpolate: true,
            intent: CGColorRenderingIntent::RenderingIntentDefault,
            mask: false,
            derived: Derived::None,
            file_type: None,
        }
    }
}

fn make(layout: Layout, provider: Option<Retained<CGDataProviderImpl>>, made: Made) -> Retained<CGImageImpl> {
    let Made { interpolate, intent, mask, derived, file_type } = made;
    let ivars = ImageIvars {
        layout,
        provider,
        interpolate,
        intent,
        mask,
        derived,
        file_type,
        key: crate::raster::images::next_key(),
        pixels: OnceLock::new(),
        rgba: OnceLock::new(),
        clip: OnceLock::new(),
        part_provider: OnceLock::new(),
        recorded: AtomicBool::new(false),
    };
    let this = CGImageImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// Whether `layout` is one images come in and `provider` has the bytes
/// for it (as far as it knows).
fn fits(layout: &Layout, provider: &CGDataProviderImpl) -> bool {
    layout.valid() && provider.known_len().is_none_or(|n| layout.len().is_some_and(|need| n >= need))
}

/// A new image of `layout`, its pixels `provider`'s, if it fits.
pub(crate) fn new_image(layout: Layout, provider: Retained<CGDataProviderImpl>) -> Option<Retained<CGImageImpl>> {
    fits(&layout, &provider).then(|| make(layout, Some(provider), Made::plain()))
}

// Drawing.

/// Draw `image`, whose pixels are `data` (worked out by the caller before
/// it took the state), into `rect` (user space), its top row at the
/// rectangle's top (its maximum y), or tiled from there over the clip.
pub(crate) fn draw(st: &mut ContextState, rect: CGRect, image: &CGImageImpl, data: Arc<ImageData>, tiled: bool) {
    let (x0, y0, x1, y1) = super::geometry::edges(rect);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let tint = image.is_mask().then_some(st.gs.fill);
    let (w, h) = (data.width as f32, data.height as f32);
    let op = Op::Image {
        image: data,
        src: Rect::new(0.0, 0.0, w, h),
        dst: Rect::new(x0 as f32, y1 as f32, x1 as f32, y0 as f32),
        alpha: 1.0,
        quality: quality_of(st),
        tint,
        tiled,
        draw: st.gs.draw(),
    };
    st.push(op);
    if matches!(st.target, crate::context::Target::Record) {
        image.ivars().recorded.store(true, Ordering::Relaxed);
    }
}

/// The interpolation images are drawn with now.
fn quality_of(st: &ContextState) -> Quality {
    match st.gs.interpolation.0 {
        1 => Quality::None,
        2 => Quality::Low,
        3 => Quality::High,
        _ => Quality::Medium,
    }
}

/// Narrow the clip by a mask's coverage stretched over `rect` (user
/// space), sampled as images are drawn now: nothing outside it.
pub(crate) fn clip_to_mask(st: &mut ContextState, rect: CGRect, mask: Arc<ImageData>) {
    let (x0, y0, x1, y1) = super::geometry::edges(rect);
    let Some(r) = tiny_skia::Rect::from_ltrb(x0 as f32, y0 as f32, x1 as f32, y1 as f32) else {
        st.gs.clip = st.gs.clip.intersect(&crate::context::map_rect_any(st.gs.ctm, super::geometry::standardize(rect)));
        st.sync();
        return;
    };
    let path = Arc::new(tiny_skia::PathBuilder::from_rect(r));
    let xf = crate::context::to_skia(st.gs.ctm);
    let bounds =
        path.bounds().transform(xf).map_or(Rect::NOWHERE, |b| Rect::new(b.left(), b.top(), b.right(), b.bottom()));
    let image = crate::protocol::ClipImage {
        image: mask,
        dst: Rect::new(x0 as f32, y1 as f32, x1 as f32, y0 as f32),
        quality: quality_of(st),
    };
    let clip = ClipPath { path, even_odd: false, xf, aa: st.gs.aa && st.allows_aa, image: Some(Arc::new(image)) };
    st.gs.clip = st.gs.clip.intersect(&bounds);
    let mut paths: Vec<ClipPath> = st.gs.mask.as_deref().map(<[ClipPath]>::to_vec).unwrap_or_default();
    paths.push(clip);
    st.gs.mask = Some(paths.into());
    st.sync();
}

// The functions.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_IMAGE
}

fn decode_array(decode: *const CGFloat, n: usize) -> Option<Vec<f64>> {
    // SAFETY: the caller of the public functions passes null or 2 values a
    // component.
    (!decode.is_null()).then(|| unsafe { std::slice::from_raw_parts(decode, 2 * n) }.to_vec())
}

/// # Safety
///
/// `decode` is null or holds two values a sample.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGImageCreate(
    width: usize,
    height: usize,
    bits_per_component: usize,
    bits_per_pixel: usize,
    bytes_per_row: usize,
    space: Option<&CGColorSpace>,
    bitmap_info: CGBitmapInfo,
    provider: Option<&CGDataProvider>,
    decode: *const CGFloat,
    should_interpolate: bool,
    intent: CGColorRenderingIntent,
) -> Option<NonNull<CGImage>> {
    let space = space_imp(space?).retain();
    let provider = provider_imp(provider?).retain();
    let mut info = bitmap_info.0;
    // Floats with the default byte order are big-endian, and say so, as
    // CoreGraphics' do.
    if info & FLOAT != 0 && info & ORDER_MASK == 0 {
        info |= if bits_per_component == 16 { ORDER_16_BIG } else { ORDER_32_BIG };
    }
    let mut layout = Layout {
        width,
        height,
        bpc: bits_per_component,
        bpp: bits_per_pixel,
        bpr: bytes_per_row,
        space: Some(space),
        info,
        decode: None,
    };
    layout.decode = decode_array(decode, layout.samples());
    if !fits(&layout, &provider) {
        return None;
    }
    let made = Made { interpolate: should_interpolate, intent, ..Made::plain() };
    Some(super::owned(make(layout, Some(provider), made)))
}

/// # Safety
///
/// `decode` is null or holds two values.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGImageMaskCreate(
    width: usize,
    height: usize,
    bits_per_component: usize,
    bits_per_pixel: usize,
    bytes_per_row: usize,
    provider: Option<&CGDataProvider>,
    decode: *const CGFloat,
    should_interpolate: bool,
) -> Option<NonNull<CGImage>> {
    let provider = provider_imp(provider?).retain();
    if !matches!(bits_per_component, 1 | 2 | 4 | 8) || bits_per_pixel < bits_per_component {
        return None;
    }
    let layout = Layout {
        width,
        height,
        bpc: bits_per_component,
        bpp: bits_per_pixel,
        bpr: bytes_per_row,
        space: None,
        info: 0,
        decode: decode_array(decode, 1),
    };
    if !fits(&layout, &provider) {
        return None;
    }
    let made = Made { interpolate: should_interpolate, mask: true, ..Made::plain() };
    Some(super::owned(make(layout, Some(provider), made)))
}

/// An image mask of `layout` (one sample a pixel) over `provider`, for a
/// bitmap context of alpha alone: its alpha is the mask's coverage, so the
/// decode array turns it over.
pub(crate) fn alpha_mask(mut layout: Layout, provider: Retained<CGDataProviderImpl>) -> Option<Retained<CGImageImpl>> {
    layout.space = None;
    layout.info = 0;
    layout.decode = Some(vec![1.0, 0.0]);
    fits(&layout, &provider).then(|| make(layout, Some(provider), Made { mask: true, ..Made::plain() }))
}

/// A copy of `image` in `layout`, of `derived` (or what it's of). A copy
/// in its image's layout of a file is the file's too, drawn where it's
/// drawn.
fn copy_of(image: &CGImageImpl, layout: Layout, derived: Option<Derived>) -> Retained<CGImageImpl> {
    let i = image.ivars();
    let same = derived.is_none() && same_layout(&layout, &i.layout);
    let derived = derived.unwrap_or_else(|| i.derived.clone());
    let made = Made { interpolate: i.interpolate, intent: i.intent, mask: i.mask, derived, file_type: i.file_type };
    let copy = make(layout, i.provider.clone(), made);
    if same
        && let (Some(Some(data)), Derived::None) = (i.pixels.get(), &copy.ivars().derived)
        && matches!(data.pixels, Pixels::Encoded(..))
    {
        let _ = copy.ivars().pixels.set(Some(Arc::new(ImageData {
            key: copy.ivars().key,
            generation: 0,
            width: data.width,
            height: data.height,
            pixels: match &data.pixels {
                Pixels::Encoded(file, upright) => Pixels::Encoded(file.clone(), *upright),
                _ => unreachable!(),
            },
        })));
    }
    copy
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageCreateCopy(image: Option<&CGImage>) -> Option<NonNull<CGImage>> {
    let image = image_imp(image?);
    Some(super::owned(copy_of(image, image.layout().clone(), None)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageCreateCopyWithColorSpace(
    image: Option<&CGImage>,
    space: Option<&CGColorSpace>,
) -> Option<NonNull<CGImage>> {
    let (image, space) = (image_imp(image?), space_imp(space?));
    if image.is_mask() || space.info().components != image.layout().colors() {
        return None;
    }
    let layout = Layout { space: Some(space.retain()), ..image.layout().clone() };
    Some(super::owned(copy_of(image, layout, None)))
}

/// Whether two layouts are the same: the same pixels drawn the same.
fn same_layout(a: &Layout, b: &Layout) -> bool {
    let space = match (&a.space, &b.space) {
        (Some(x), Some(y)) => std::ptr::eq(&**x, &**y),
        (None, None) => true,
        _ => false,
    };
    space
        && (a.width, a.height, a.bpc, a.bpp, a.bpr, a.info) == (b.width, b.height, b.bpc, b.bpp, b.bpr, b.info)
        && a.decode == b.decode
}

/// The headroom CoreGraphics assumes for HDR content that doesn't state
/// its own, as macOS gives it.
#[unsafe(no_mangle)]
pub static kCGDefaultHDRImageContentHeadroom: f32 = 4.926_108_4;

// The file types CoreGraphics names images of files by.
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepUTTypePNG = "public.png"
);
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepUTTypeJPEG = "public.jpeg"
);

/// Which kind of file bytes are, by their signature.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Png,
    Jpeg,
}

fn kind_of(file: &[u8]) -> Option<FileKind> {
    if file.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(FileKind::Png)
    } else if file.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(FileKind::Jpeg)
    } else {
        None
    }
}

/// An image of an encoded file's pixels: 8-bit RGBA, alpha straight
/// (`Last`), or skipped for a file without alpha; turned upright by its
/// orientation, or as stored. Only the header is read now: drawing decodes
/// the file where it rasterizes (the render thread, for windows), and the
/// provider when its bytes are asked for.
pub(crate) fn from_file(file: Arc<[u8]>, upright: bool, interpolate: bool) -> Option<Retained<CGImageImpl>> {
    let file_type: Option<&'static sidestep_runtime::ObjectRef> = match kind_of(&file) {
        Some(FileKind::Png) => Some(&_SidestepUTTypePNG),
        Some(FileKind::Jpeg) => Some(&_SidestepUTTypeJPEG),
        None => None,
    };
    from_file_of_type(file, upright, interpolate, file_type)
}

/// [`from_file`], naming the file's type as `file_type` (for ImageIO,
/// whose images name every type they come from).
pub(crate) fn from_file_of_type(
    file: Arc<[u8]>,
    upright: bool,
    interpolate: bool,
    file_type: Option<&'static sidestep_runtime::ObjectRef>,
) -> Option<Retained<CGImageImpl>> {
    let header = crate::codec::header(&file)?;
    let (mut w, mut h) = (header.width as usize, header.height as usize);
    if upright && header.orientation.swaps() {
        (w, h) = (h, w);
    }
    let layout = Layout {
        width: w,
        height: h,
        bpc: 8,
        bpp: 32,
        bpr: w.checked_mul(4)?,
        space: Some(super::color::srgb()),
        info: if header.alpha { CGImageAlphaInfo::Last.0 } else { CGImageAlphaInfo::NoneSkipLast.0 },
        decode: None,
    };
    let provider = super::data::of_file(file.clone(), upright);
    if !fits(&layout, &provider) {
        return None;
    }
    let image = make(layout, Some(provider), Made { interpolate, file_type, ..Made::plain() });
    let data = ImageData {
        key: image.ivars().key,
        generation: 0,
        width: w as u32,
        height: h as u32,
        pixels: Pixels::Encoded(file.clone(), upright),
    };
    let _ = image.ivars().pixels.set(Some(Arc::new(data)));
    Some(image)
}

/// An image of the file `source` holds, if it's a `kind` file.
fn from_provider(source: Option<&CGDataProvider>, kind: FileKind, interpolate: bool) -> Option<NonNull<CGImage>> {
    let file = provider_imp(source?).bytes()?;
    if kind_of(&file) != Some(kind) {
        return None;
    }
    from_file(file, false, interpolate).map(super::owned)
}

/// # Safety
///
/// `decode` is null or holds two values a component.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGImageCreateWithJPEGDataProvider(
    source: Option<&CGDataProvider>,
    _decode: *const CGFloat,
    should_interpolate: bool,
    _intent: CGColorRenderingIntent,
) -> Option<NonNull<CGImage>> {
    from_provider(source, FileKind::Jpeg, should_interpolate)
}

/// # Safety
///
/// `decode` is null or holds two values a component.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGImageCreateWithPNGDataProvider(
    source: Option<&CGDataProvider>,
    _decode: *const CGFloat,
    should_interpolate: bool,
    _intent: CGColorRenderingIntent,
) -> Option<NonNull<CGImage>> {
    from_provider(source, FileKind::Png, should_interpolate)
}

/// Part of an image: `rect` (pixels, from the top left) made whole, within
/// the image; nothing if that's empty.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageCreateWithImageInRect(
    image: Option<&CGImage>,
    rect: CGRect,
) -> Option<NonNull<CGImage>> {
    let image = image_imp(image?);
    let l = image.layout();
    let bounds = super::geometry::rect(0.0, 0.0, l.width as f64, l.height as f64);
    let r = super::geometry::CGRectIntersection(super::geometry::CGRectIntegral(rect), bounds);
    if super::geometry::CGRectIsEmpty(r) {
        return None;
    }
    let (x, y) = (r.origin.x as usize, r.origin.y as usize);
    let layout = Layout { width: r.size.width as usize, height: r.size.height as usize, ..l.clone() };
    let i = image.ivars();
    let derived = Derived::Part(image.retain(), x, y);
    let made = Made { interpolate: i.interpolate, intent: i.intent, mask: i.mask, derived, file_type: None };
    Some(super::owned(make(layout, None, made)))
}

/// An image masked by an image mask or a gray image without alpha.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageCreateWithMask(
    image: Option<&CGImage>,
    mask: Option<&CGImage>,
) -> Option<NonNull<CGImage>> {
    let (image, mask) = (image_imp(image?), image_imp(mask?));
    let gray = mask.layout().space.as_ref().is_some_and(|s| s.info().model == Model::Gray)
        && mask.layout().alpha() == CGImageAlphaInfo::None;
    if image.is_mask() || !(mask.is_mask() || gray) {
        return None;
    }
    let layout = image.layout().clone();
    Some(super::owned(copy_of(image, layout, Some(Derived::Masked(image.retain(), mask.retain())))))
}

/// # Safety
///
/// `components` holds a minimum and maximum a color component.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGImageCreateWithMaskingColors(
    image: Option<&CGImage>,
    components: *const CGFloat,
) -> Option<NonNull<CGImage>> {
    let image = image_imp(image?);
    let l = image.layout();
    if image.is_mask() || components.is_null() || l.alpha() != CGImageAlphaInfo::None {
        return None;
    }
    // SAFETY: as the caller promises.
    let ranges = unsafe { std::slice::from_raw_parts(components, 2 * l.colors()) }.to_vec();
    let layout = l.clone();
    Some(super::owned(copy_of(image, layout, Some(Derived::ColorMasked(image.retain(), ranges)))))
}

/// # Safety
///
/// As `CGImageCreate`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGImageCreateWithContentHeadroom(
    _headroom: c_float,
    width: usize,
    height: usize,
    bits_per_component: usize,
    bits_per_pixel: usize,
    bytes_per_row: usize,
    space: Option<&CGColorSpace>,
    bitmap_info: CGBitmapInfo,
    provider: Option<&CGDataProvider>,
    decode: *const CGFloat,
    should_interpolate: bool,
    intent: CGColorRenderingIntent,
) -> Option<NonNull<CGImage>> {
    // SAFETY: as the caller promises.
    unsafe {
        CGImageCreate(
            width,
            height,
            bits_per_component,
            bits_per_pixel,
            bytes_per_row,
            space,
            bitmap_info,
            provider,
            decode,
            should_interpolate,
            intent,
        )
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageCreateCopyWithContentHeadroom(
    _headroom: c_float,
    image: Option<&CGImage>,
) -> Option<NonNull<CGImage>> {
    CGImageCreateCopy(image)
}

/// Images here have standard dynamic range: no headroom.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetContentHeadroom(_image: Option<&CGImage>) -> c_float {
    1.0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageCalculateContentHeadroom(_image: Option<&CGImage>) -> c_float {
    1.0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageIsMask(image: Option<&CGImage>) -> bool {
    image.is_some_and(|i| image_imp(i).is_mask())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetWidth(image: Option<&CGImage>) -> usize {
    image.map_or(0, |i| image_imp(i).layout().width)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetHeight(image: Option<&CGImage>) -> usize {
    image.map_or(0, |i| image_imp(i).layout().height)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetBitsPerComponent(image: Option<&CGImage>) -> usize {
    image.map_or(0, |i| image_imp(i).layout().bpc)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetBitsPerPixel(image: Option<&CGImage>) -> usize {
    image.map_or(0, |i| image_imp(i).layout().bpp)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetBytesPerRow(image: Option<&CGImage>) -> usize {
    image.map_or(0, |i| image_imp(i).layout().bpr)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetColorSpace(image: Option<&CGImage>) -> Option<NonNull<CGColorSpace>> {
    image_imp(image?).layout().space.as_deref().map(super::borrowed)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetAlphaInfo(image: Option<&CGImage>) -> CGImageAlphaInfo {
    image.map_or(CGImageAlphaInfo::None, |i| image_imp(i).layout().alpha())
}

/// The provider the image's bytes come from; a part's is one of its own,
/// its rows of its image's bytes, made the first time it's asked for.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetDataProvider(image: Option<&CGImage>) -> Option<NonNull<CGDataProvider>> {
    let image = image_imp(image?);
    let provider = match &image.ivars().derived {
        Derived::Part(..) => {
            image.part_provider()?;
            image.ivars().part_provider.get()?.as_deref()?
        }
        Derived::Masked(p, _) | Derived::ColorMasked(p, _) => {
            image.ivars().provider.as_deref().or_else(|| p.ivars().provider.as_deref())?
        }
        Derived::None => image.ivars().provider.as_deref()?,
    };
    Some(super::borrowed(provider))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetDecode(image: Option<&CGImage>) -> *const CGFloat {
    image.and_then(|i| image_imp(i).layout().decode.as_ref().map(|d| d.as_ptr())).unwrap_or(std::ptr::null())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetShouldInterpolate(image: Option<&CGImage>) -> bool {
    image.is_some_and(|i| image_imp(i).ivars().interpolate)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetRenderingIntent(image: Option<&CGImage>) -> CGColorRenderingIntent {
    image.map_or(CGColorRenderingIntent::RenderingIntentDefault, |i| image_imp(i).ivars().intent)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetBitmapInfo(image: Option<&CGImage>) -> CGBitmapInfo {
    CGBitmapInfo(image.map_or(0, |i| image_imp(i).layout().info))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetByteOrderInfo(image: Option<&CGImage>) -> CGImageByteOrderInfo {
    CGImageByteOrderInfo(image.map_or(0, |i| image_imp(i).layout().info & ORDER_MASK))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetPixelFormatInfo(image: Option<&CGImage>) -> CGImagePixelFormatInfo {
    CGImagePixelFormatInfo(image.map_or(0, |i| image_imp(i).layout().info & FORMAT_MASK))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageShouldToneMap(_image: Option<&CGImage>) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageContainsImageSpecificToneMappingMetadata(_image: Option<&CGImage>) -> bool {
    false
}

/// An image of a PNG or JPEG file names its type (`public.png`,
/// `public.jpeg`); others have none.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGImageGetUTType(image: Option<&CGImage>) -> Option<NonNull<CFString>> {
    let constant = image_imp(image?).ivars().file_type?;
    // SAFETY: an ObjectRef is a pointer to an immortal object, laid out as
    // one.
    NonNull::new(unsafe { *(constant as *const sidestep_runtime::ObjectRef).cast::<*mut CFString>() })
}

/// A CGImage of pixels worked out already, in sRGB: 8-bit samples, 32 bits
/// a pixel with alpha or padding first or last as `alpha` says (in the
/// default byte order), `bytes` as its provider hands them out, and `rgba`
/// the same pixels premultiplied, as drawing takes them.
pub(crate) fn from_worked_out(
    width: usize,
    height: usize,
    alpha: CGImageAlphaInfo,
    bytes: Arc<[u8]>,
    rgba: Arc<[u8]>,
    file_type: Option<&'static sidestep_runtime::ObjectRef>,
) -> Option<Retained<CGImageImpl>> {
    let layout = Layout {
        width,
        height,
        bpc: 8,
        bpp: 32,
        bpr: width.checked_mul(4)?,
        space: Some(super::color::srgb()),
        info: alpha.0,
        decode: None,
    };
    let provider = super::data::of_bytes(bytes);
    if !fits(&layout, &provider) || rgba.len() != width * height * 4 {
        return None;
    }
    let image = make(layout, Some(provider), Made { file_type, ..Made::plain() });
    let _ = image.ivars().rgba.set(Some(rgba));
    Some(image)
}

/// The layout a CGImage of 8-bit premultiplied RGBA rows `width × 4` bytes
/// apart has.
pub(crate) fn rgba_layout(width: usize, height: usize, space: Retained<CGColorSpaceImpl>) -> Layout {
    Layout {
        width,
        height,
        bpc: 8,
        bpp: 32,
        bpr: width * 4,
        space: Some(space),
        info: CGImageAlphaInfo::PremultipliedLast.0,
        decode: None,
    }
}

/// A CGImage of premultiplied RGBA pixels (rows `width × 4` bytes apart)
/// in `space`, drawn as they are.
pub(crate) fn from_rgba(
    width: usize,
    height: usize,
    rgba: Arc<[u8]>,
    space: Retained<CGColorSpaceImpl>,
) -> Option<Retained<CGImageImpl>> {
    let image = new_image(rgba_layout(width, height, space), super::data::of_bytes(rgba.clone()))?;
    let _ = image.ivars().rgba.set(Some(rgba));
    Some(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(bpc: usize, bpp: usize, info: u32, space: &str) -> Layout {
        Layout {
            width: 1,
            height: 1,
            bpc,
            bpp,
            bpr: bpp.div_ceil(8),
            space: super::super::color::named(space),
            info,
            decode: None,
        }
    }

    #[test]
    fn layouts_decode_to_premultiplied_rgba() {
        let srgb = "kCGColorSpaceSRGB";
        let gray = "kCGColorSpaceDeviceGray";
        // BGRA, alpha first in a little-endian word.
        let l = layout(8, 32, CGImageAlphaInfo::PremultipliedFirst.0 | ORDER_32_LITTLE, srgb);
        assert_eq!(decode(&l, &[255, 0, 0, 255]), [0, 0, 255, 255]);
        // Straight alpha is premultiplied.
        assert_eq!(decode(&layout(8, 32, CGImageAlphaInfo::Last.0, srgb), &[255, 0, 0, 128]), [128, 0, 0, 128]);
        // Padding is ignored, whatever it holds.
        assert_eq!(
            decode(&layout(8, 32, CGImageAlphaInfo::NoneSkipFirst.0, srgb), &[0, 10, 20, 30]),
            [10, 20, 30, 255]
        );
        // 16-bit samples, big-endian by default.
        let l = layout(16, 48, 0, srgb);
        assert_eq!(decode(&l, &[0xff, 0xff, 0x80, 0x00, 0, 0]), [255, 128, 0, 255]);
        // Half floats.
        let l = layout(16, 48, FLOAT | ORDER_16_BIG, srgb);
        assert_eq!(decode(&l, &[0x3c, 0, 0x38, 0, 0, 0]), [255, 128, 0, 255]);
        // Packed gray.
        let l = Layout { width: 2, ..layout(4, 4, 0, gray) };
        assert_eq!(decode(&l, &[0x8f]), [136, 136, 136, 255, 255, 255, 255, 255]);
        // A decode array maps its range onto 0 to 1, and leaves alpha.
        let l = Layout { decode: Some(vec![0.0, 0.5, 0.0, 1.0]), ..layout(8, 16, CGImageAlphaInfo::Last.0, gray) };
        assert_eq!(decode(&l, &[64, 255]), [128, 128, 128, 255]);
    }
}
