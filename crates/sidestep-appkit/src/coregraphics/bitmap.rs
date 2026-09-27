//! Bitmap contexts: `CGBitmapContextCreate` and its getters.
//!
//! A bitmap context draws into memory, the program's or its own, in the
//! layout it asks for, rasterizing each op at once on the calling thread
//! (`raster::pixels`): 8-bit RGB with premultiplied alpha or a padding
//! sample (holding alpha as CoreGraphics writes it, the colors not held to
//! it)
//! first or last, in either byte order within the 32-bit pixel; 16-bit and
//! 32-bit float RGB with alpha or padding last; 8, 16 or 32-bit float gray,
//! 8-bit gray with alpha or padding; 8-bit alpha alone. Multi-byte samples
//! are big-endian unless the byte order says little, as CoreGraphics lays
//! them out; a byte order must fit the samples (16-bit orders for 16-bit
//! samples or 8-bit pairs, 32-bit ones for 8-bit quads and floats), and
//! extended spaces take floats, as CoreGraphics requires. Rows of the
//! context's own memory are 32-byte aligned when no row length is given;
//! the program's memory needs a row length, which must hold a row and be a
//! whole number of pixels. Other layouts make no context (as CoreGraphics'
//! unsupported ones make none); CMYK and 5-bit RGB, which CoreGraphics
//! draws into, aren't here. The release callback runs when the context
//! goes, with the program's memory or the context's own.
//!
//! The context's coordinates are CoreGraphics': user space starts as the
//! bitmap's pixels with the origin at the bottom left, y up; layer points
//! (the ops') are the pixels from the top left.

use std::ffi::c_void;
use std::ptr::NonNull;

use kurbo::Affine;
use objc2::DefinedClass;
use objc2::rc::Retained;
use objc2_core_graphics::{
    CGBitmapContextReleaseDataCallback, CGBitmapInfo, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
};

use super::color::{CGColorSpaceImpl, Model, space_imp};
use super::context::{CGContextImpl, imp};
use super::info::{
    ALPHA_MASK, FLOAT, FORMAT_MASK, ORDER_16_BIG, ORDER_16_LITTLE, ORDER_32_BIG, ORDER_32_LITTLE, ORDER_MASK,
};
use crate::context::{ContextState, Target};
use crate::protocol::{Op, Rect};
use crate::raster::pixels::{Colors, Format, Memory, Sample};

/// A bitmap context's memory and what it reports of it.
pub(crate) struct Surface {
    pub mem: Memory,
    /// The memory, when the context allocated it (kept here for the
    /// context's life).
    _owned: Option<Vec<u32>>,
    pub bpc: usize,
    pub bpp: usize,
    pub info: u32,
    pub space: Option<Retained<CGColorSpaceImpl>>,
    /// What to call with the program's memory when the context goes.
    release: CGBitmapContextReleaseDataCallback,
    release_info: *mut c_void,
}

impl Surface {
    pub(crate) fn rasterize(&mut self, ops: &[Op]) {
        // SAFETY: the memory is the context's own buffer or the program's,
        // which holds bpr × height bytes as CGBitmapContextCreate requires,
        // and the context is used by one thread at a time.
        unsafe { crate::raster::pixels::rasterize(&self.mem, 1.0, ops) };
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        if let Some(release) = self.release {
            // SAFETY: the program's callback, with the memory it lent or
            // the context's own (freed after), as CoreGraphics calls it.
            unsafe { release(self.release_info, self.mem.base.cast()) };
        }
    }
}

/// The pixel format a context draws in for these parameters, and its
/// bits a pixel; `None` for layouts that make no context.
fn format_of(bpc: usize, space: Option<&CGColorSpaceImpl>, info: u32) -> Option<(Format, usize)> {
    let alpha = CGImageAlphaInfo(info & ALPHA_MASK);
    let order = info & ORDER_MASK;
    let float = info & FLOAT != 0;
    if info & FORMAT_MASK != 0 {
        return None;
    }
    let kind = match (bpc, float) {
        (8, false) => Sample::U8,
        (16, false) => Sample::U16,
        (32, true) => Sample::F32,
        _ => return None,
    };
    // Extended spaces take floats.
    if space.is_some_and(|s| s.info().extended) && kind != Sample::F32 {
        return None;
    }
    let big = match kind {
        Sample::U8 => false,
        _ => !matches!(order, ORDER_16_LITTLE | ORDER_32_LITTLE),
    };
    let model = space.map(|s| s.info().model);
    // Slots, colors, the colors' slots, alpha's (or padding's) slot, and
    // whether that's padding.
    let (slots, colors, order_slots, alpha_slot, padding) = match (model, alpha) {
        // Alpha alone: the space doesn't count.
        (_, CGImageAlphaInfo::Only) if kind == Sample::U8 => (1, Colors::None, [0; 3], Some(0), false),
        (Some(Model::Gray), CGImageAlphaInfo::None) => (1, Colors::Gray, [0; 3], None, false),
        (Some(Model::Gray), CGImageAlphaInfo::PremultipliedLast) if kind == Sample::U8 => {
            (2, Colors::Gray, [0; 3], Some(1), false)
        }
        (Some(Model::Gray), CGImageAlphaInfo::NoneSkipLast) if kind == Sample::U8 => {
            (2, Colors::Gray, [0; 3], Some(1), true)
        }
        (Some(Model::Rgb), CGImageAlphaInfo::PremultipliedLast | CGImageAlphaInfo::NoneSkipLast) => {
            (4, Colors::Rgb, [0, 1, 2], Some(3), alpha == CGImageAlphaInfo::NoneSkipLast)
        }
        (Some(Model::Rgb), CGImageAlphaInfo::PremultipliedFirst | CGImageAlphaInfo::NoneSkipFirst)
            if kind == Sample::U8 =>
        {
            (4, Colors::Rgb, [1, 2, 3], Some(0), alpha == CGImageAlphaInfo::NoneSkipFirst)
        }
        _ => return None,
    };
    // The byte order must fit the samples: 16-bit orders for 16-bit
    // samples or pairs of 8-bit ones, 32-bit orders for quads of 8-bit ones
    // and floats.
    let order_fits = match (kind, order) {
        (_, 0) => true,
        (Sample::U8, ORDER_16_LITTLE | ORDER_16_BIG) => slots == 2,
        (Sample::U8, ORDER_32_LITTLE | ORDER_32_BIG) => slots == 4,
        (Sample::U16, ORDER_16_LITTLE | ORDER_16_BIG) => true,
        (Sample::F32, ORDER_32_LITTLE | ORDER_32_BIG) => true,
        _ => false,
    };
    if !order_fits {
        return None;
    }
    // 8-bit samples in a little-endian pixel word come in reverse order.
    let reverse =
        kind == Sample::U8 && ((order == ORDER_32_LITTLE && slots == 4) || (order == ORDER_16_LITTLE && slots == 2));
    let flip = |s: usize| if reverse { slots - 1 - s } else { s };
    let format =
        Format { kind, slots, colors, order: order_slots.map(flip), alpha: alpha_slot.map(flip), padding, big };
    Some((format, slots * bpc))
}

/// A bitmap context over `data` (or memory of its own) in the layout asked
/// for.
#[allow(clippy::too_many_arguments)]
fn create(
    data: *mut c_void,
    width: usize,
    height: usize,
    bpc: usize,
    bpr: usize,
    space: Option<&CGColorSpace>,
    info: u32,
    release: CGBitmapContextReleaseDataCallback,
    release_info: *mut c_void,
) -> Option<Retained<CGContextImpl>> {
    let space = space.map(space_imp);
    if width == 0 || height == 0 {
        return None;
    }
    let (format, bpp) = format_of(bpc, space, info)?;
    let bytes = bpp / 8;
    let row = width.checked_mul(bytes)?;
    let bpr = match bpr {
        // Rows of the context's own memory are aligned; the program's
        // memory must say how long its rows are, as CoreGraphics requires.
        0 if data.is_null() => row.checked_next_multiple_of(32)?,
        b if b != 0 && b >= row && b.is_multiple_of(bytes) => b,
        _ => return None,
    };
    let len = bpr.checked_mul(height)?;
    if len > isize::MAX as usize {
        return None;
    }
    let (base, owned) = if data.is_null() {
        let mut buffer = crate::bitmap::zeroed_words(len.div_ceil(4))?;
        (buffer.as_mut_ptr().cast::<u8>(), Some(buffer))
    } else {
        (data.cast::<u8>(), None)
    };
    let alpha_only = format.colors == Colors::None;
    let surface = Surface {
        mem: Memory { base, width, height, bpr, format },
        _owned: owned,
        bpc,
        bpp,
        info,
        space: if alpha_only { None } else { space.map(objc2::Message::retain) },
        release,
        release_info,
    };
    let (w, h) = (width as f64, height as f64);
    // User space starts as the pixels from the bottom left, y up; layer
    // points are the pixels from the top left.
    let base = Affine::new([1.0, 0.0, 0.0, -1.0, 0.0, h]);
    let clip = Rect::new(0.0, 0.0, w as f32, h as f32);
    let state = ContextState::new(Target::Surface(Box::new(surface)), false, base, clip, 1.0, base);
    Some(super::context::new_context(state))
}

/// # Safety
///
/// `data` is null or holds `bytes_per_row × height` bytes for the
/// context's life (a context over the program's memory needs
/// `bytes_per_row`).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGBitmapContextCreate(
    data: *mut c_void,
    width: usize,
    height: usize,
    bits_per_component: usize,
    bytes_per_row: usize,
    space: Option<&CGColorSpace>,
    bitmap_info: u32,
) -> Option<NonNull<CGContext>> {
    create(data, width, height, bits_per_component, bytes_per_row, space, bitmap_info, None, std::ptr::null_mut())
        .map(super::owned)
}

/// # Safety
///
/// As `CGBitmapContextCreate`; `release_callback` is valid for
/// `release_info` and the data.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGBitmapContextCreateWithData(
    data: *mut c_void,
    width: usize,
    height: usize,
    bits_per_component: usize,
    bytes_per_row: usize,
    space: Option<&CGColorSpace>,
    bitmap_info: u32,
    release_callback: CGBitmapContextReleaseDataCallback,
    release_info: *mut c_void,
) -> Option<NonNull<CGContext>> {
    create(data, width, height, bits_per_component, bytes_per_row, space, bitmap_info, release_callback, release_info)
        .map(super::owned)
}

/// What a bitmap context (its own, or an `NSBitmapImageRep`'s) reports:
/// memory, bits a component and a pixel, bitmap info and space.
struct Described {
    mem: Memory,
    bpc: usize,
    bpp: usize,
    info: u32,
    space: Option<Retained<CGColorSpaceImpl>>,
}

fn describe(c: Option<&CGContext>) -> Option<Described> {
    let st = imp(c?).ivars().state.try_borrow().ok()?;
    match &st.target {
        Target::Surface(s) => {
            Some(Described { mem: s.mem, bpc: s.bpc, bpp: s.bpp, info: s.info, space: s.space.clone() })
        }
        Target::Bitmap(rep) => {
            let mem = crate::bitmap::memory(rep)?;
            let f = mem.format;
            let bpc = f.kind.bytes() * 8;
            let alpha = match (f.alpha, f.colors, f.padding) {
                (Some(0), Colors::Rgb, false) => CGImageAlphaInfo::PremultipliedFirst,
                (Some(0), Colors::Rgb, true) => CGImageAlphaInfo::NoneSkipFirst,
                (Some(_), _, false) => CGImageAlphaInfo::PremultipliedLast,
                (Some(_), _, true) | (None, Colors::Rgb, _) => CGImageAlphaInfo::NoneSkipLast,
                (None, ..) => CGImageAlphaInfo::None,
            };
            let order = match (f.kind, cfg!(target_endian = "little")) {
                (Sample::U16, true) => ORDER_16_LITTLE,
                (Sample::F32, true) => ORDER_32_LITTLE,
                _ => 0,
            };
            let float = if f.kind == Sample::F32 { FLOAT } else { 0 };
            let name = if f.colors == Colors::Gray { "kCGColorSpaceDeviceGray" } else { "kCGColorSpaceDeviceRGB" };
            Some(Described {
                mem,
                bpc,
                bpp: f.slots * bpc,
                info: alpha.0 | order | float,
                space: super::color::named(name),
            })
        }
        Target::Record => None,
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGBitmapContextGetData(context: Option<&CGContext>) -> *mut c_void {
    describe(context).map_or(std::ptr::null_mut(), |d| d.mem.base.cast())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGBitmapContextGetWidth(context: Option<&CGContext>) -> usize {
    describe(context).map_or(0, |d| d.mem.width)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGBitmapContextGetHeight(context: Option<&CGContext>) -> usize {
    describe(context).map_or(0, |d| d.mem.height)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGBitmapContextGetBitsPerComponent(context: Option<&CGContext>) -> usize {
    describe(context).map_or(0, |d| d.bpc)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGBitmapContextGetBitsPerPixel(context: Option<&CGContext>) -> usize {
    describe(context).map_or(0, |d| d.bpp)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGBitmapContextGetBytesPerRow(context: Option<&CGContext>) -> usize {
    describe(context).map_or(0, |d| d.mem.bpr)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGBitmapContextGetColorSpace(context: Option<&CGContext>) -> Option<NonNull<CGColorSpace>> {
    let st = imp(context?).ivars().state.try_borrow().ok()?;
    match &st.target {
        Target::Surface(s) => s.space.as_deref().map(super::borrowed),
        _ => {
            drop(st);
            // The named spaces live forever, so a borrowed one stays valid.
            describe(context)?.space.as_deref().map(super::borrowed)
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGBitmapContextGetAlphaInfo(context: Option<&CGContext>) -> CGImageAlphaInfo {
    CGImageAlphaInfo(describe(context).map_or(0, |d| d.info & ALPHA_MASK))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGBitmapContextGetBitmapInfo(context: Option<&CGContext>) -> CGBitmapInfo {
    CGBitmapInfo(describe(context).map_or(0, |d| d.info))
}

/// An image of what the context holds now: a copy, in its layout (for a
/// context of alpha alone, an image mask of it, as CoreGraphics makes).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGBitmapContextCreateImage(context: Option<&CGContext>) -> Option<NonNull<CGImage>> {
    super::context::with_state(context?, ContextState::flush);
    let d = describe(context)?;
    let len = d.mem.bpr * d.mem.height;
    // SAFETY: the context's memory holds bpr × height bytes.
    let bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(unsafe { std::slice::from_raw_parts(d.mem.base, len) });
    let layout = super::image::Layout {
        width: d.mem.width,
        height: d.mem.height,
        bpc: d.bpc,
        bpp: d.bpp,
        bpr: d.mem.bpr,
        space: d.space,
        info: d.info,
        decode: None,
    };
    let provider = super::data::of_bytes(bytes);
    if d.mem.format.colors == Colors::None {
        return super::image::alpha_mask(layout, provider).map(super::owned);
    }
    super::image::new_image(layout, provider).map(super::owned)
}
