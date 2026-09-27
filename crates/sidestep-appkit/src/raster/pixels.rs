//! Drawing into pixels in memory that aren't a canvas's own format, and
//! reading them: `NSBitmapImageRep`'s layouts and CoreGraphics bitmap
//! contexts' (gray, gray and alpha, alpha alone, RGB with alpha or a
//! padding sample first or last, 8-bit bytes in either order within a
//! 32-bit pixel, 16-bit and 32-bit floating-point samples in either byte
//! order).
//!
//! [`rasterize`] draws ops into such memory on the calling thread: straight
//! into it when it holds canvas pixels (8-bit RGBA, premultiplied, alpha
//! last, 4-byte aligned rows), and otherwise through a scratch canvas of
//! just the pixels the ops can reach, unpacked from the memory and packed
//! back where they changed, so pixels nothing drew keep every bit of their
//! precision and an op costs what it covers, not the whole bitmap.

use std::cell::RefCell;

use super::{Canvas, Glyphs};
use crate::protocol::{Op, Rect};

/// How samples are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sample {
    U8,
    U16,
    F32,
}

impl Sample {
    pub fn bytes(self) -> usize {
        match self {
            Sample::U8 => 1,
            Sample::U16 => 2,
            Sample::F32 => 4,
        }
    }
}

/// What a pixel's samples hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Colors {
    Gray,
    Rgb,
    /// No color: alpha alone (a mask).
    None,
}

/// A pixel layout drawing can go into: `slots` samples of `kind` a pixel,
/// the colors in the slots `order` gives (red, green and blue; gray in the
/// first), and alpha in slot `alpha`; with `padding`, that slot is a
/// padding sample instead, which holds alpha as CoreGraphics writes and
/// reads it, but doesn't hold the colors to it. Multi-byte samples are stored
/// big-endian or little-endian as `big` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Format {
    pub kind: Sample,
    pub slots: usize,
    pub colors: Colors,
    /// The slot of each color (red, green, blue; gray uses the first).
    pub order: [usize; 3],
    pub alpha: Option<usize>,
    pub padding: bool,
    pub big: bool,
}

impl Format {
    /// The canvas's own format: 8-bit RGBA, alpha last.
    pub const CANVAS: Format = Format {
        kind: Sample::U8,
        slots: 4,
        colors: Colors::Rgb,
        order: [0, 1, 2],
        alpha: Some(3),
        padding: false,
        big: false,
    };

    pub fn bytes_per_pixel(&self) -> usize {
        self.slots * self.kind.bytes()
    }

    pub fn is_canvas(&self) -> bool {
        *self == Format::CANVAS
    }
}

/// Pixels in memory: `width` × `height` of `format`, rows `bpr` bytes
/// apart from `base`, the top row first.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Memory {
    pub base: *mut u8,
    pub width: usize,
    pub height: usize,
    pub bpr: usize,
    pub format: Format,
}

thread_local! {
    static GLYPHS: RefCell<Glyphs> = RefCell::default();
    /// The pixels being drawn when they aren't canvas pixels: as unpacked,
    /// and as drawn.
    static SCRATCH: RefCell<(Vec<u32>, Vec<u32>)> = RefCell::default();
}

/// Draw `ops` (in points, `scale` pixels a point, top-left origin) into
/// `mem`, on this thread.
///
/// # Safety
///
/// `mem.base` points at `bpr × height` bytes (the last row needs only
/// `width` pixels) that nothing else reads or writes meanwhile, and `bpr`
/// holds a row of pixels.
pub(crate) unsafe fn rasterize(mem: &Memory, scale: f32, ops: &[Op]) {
    if mem.width == 0 || mem.height == 0 || ops.is_empty() {
        return;
    }
    let damage = Rect::new(0.0, 0.0, mem.width as f32 / scale, mem.height as f32 / scale);
    let paint = |canvas: &mut Canvas| {
        GLYPHS.with(|g| match g.try_borrow_mut() {
            Ok(mut g) => super::paint(canvas, &mut g, &[damage], ops),
            Err(_) => super::paint(canvas, &mut Glyphs::default(), &[damage], ops),
        });
    };
    if mem.format.is_canvas() && mem.base.cast::<u32>().is_aligned() && mem.bpr.is_multiple_of(4) {
        let stride = mem.bpr / 4;
        let len = stride * (mem.height - 1) + mem.width;
        // SAFETY: the memory holds `len` u32s from `base` (as the caller
        // promises), 4-byte aligned, as checked; nothing else touches it.
        let px = unsafe { std::slice::from_raw_parts_mut(mem.base.cast::<u32>(), len) };
        let mut canvas = Canvas::new(px, mem.width as u32, mem.height as u32, 0.0, scale);
        canvas.stride = stride;
        paint(&mut canvas);
        return;
    }
    // Only the pixels the ops can reach.
    let probe = Canvas::new(&mut [], mem.width as u32, mem.height as u32, 0.0, scale);
    let Some((x0, y0, x1, y1)) = super::ops::contents(&probe, &damage, ops) else { return };
    let (w, h) = (x1 - x0, y1 - y0);
    SCRATCH.with(|scratch| {
        let mut fresh = <(Vec<u32>, Vec<u32>)>::default();
        let mut held = scratch.try_borrow_mut();
        let (before, px) = match held.as_deref_mut() {
            Ok(s) => (&mut s.0, &mut s.1),
            Err(_) => (&mut fresh.0, &mut fresh.1),
        };
        before.clear();
        before.resize(w * h, 0);
        // SAFETY: the region is inside the pixels, which the caller
        // promises are there.
        unsafe { unpack(mem, (x0, y0, w, h), before) };
        px.clear();
        px.extend_from_slice(before);
        let mut canvas = Canvas::new(px, w as u32, h as u32, 0.0, scale);
        canvas.x0 = x0 as i32;
        canvas.y0 = y0 as i32;
        paint(&mut canvas);
        // SAFETY: as above.
        unsafe { pack(mem, (x0, y0, w, h), px, before) };
    });
}

/// Read the pixels of `region` (x, y, width, height) of `mem` as
/// premultiplied RGBA canvas pixels, `width` a row.
///
/// # Safety
///
/// The region's pixels are in readable memory.
pub(crate) unsafe fn unpack(mem: &Memory, (rx, ry, w, h): (usize, usize, usize, usize), out: &mut [u32]) {
    let f = mem.format;
    let bytes = f.bytes_per_pixel();
    for (y, row) in out.chunks_exact_mut(w).take(h).enumerate() {
        for (x, p) in row.iter_mut().enumerate() {
            // SAFETY: pixel (rx + x, ry + y) is inside the memory, as the
            // caller promises.
            let px = unsafe { mem.base.add((ry + y) * mem.bpr + (rx + x) * bytes) };
            // SAFETY: sample `i` is inside the pixel.
            let get = |i: usize| unsafe { read_sample(px, f.kind, f.big, i) };
            // Padding is read as alpha, but colors aren't held to it, as
            // CoreGraphics blends them (measured on macOS): a zero padding
            // byte under a color blends with that color.
            let a = f.alpha.map_or(255, get);
            let c = |i: usize| if f.padding { get(f.order[i]) } else { get(f.order[i]).min(a) };
            *p = match f.colors {
                Colors::Gray => {
                    let g = c(0);
                    u32::from_ne_bytes([g, g, g, a])
                }
                Colors::Rgb => u32::from_ne_bytes([c(0), c(1), c(2), a]),
                Colors::None => u32::from_ne_bytes([0, 0, 0, a]),
            };
        }
    }
}

/// Write back the canvas pixels of `px` that differ from `before` into
/// `region` of `mem`: the others keep their bits.
///
/// # Safety
///
/// The region's pixels are in writable memory.
pub(crate) unsafe fn pack(mem: &Memory, (rx, ry, w, _h): (usize, usize, usize, usize), px: &[u32], before: &[u32]) {
    let f = mem.format;
    let bytes = f.bytes_per_pixel();
    for (i, (&p, _)) in px.iter().zip(before).enumerate().filter(|(_, (p, b))| p != b) {
        let (x, y) = (rx + i % w, ry + i / w);
        let [r, g, b, a] = p.to_ne_bytes();
        // SAFETY: pixel (x, y) is inside the memory, as the caller promises.
        let out = unsafe { mem.base.add(y * mem.bpr + x * bytes) };
        // SAFETY: sample `k` is inside the pixel.
        let put = |k: usize, v: u8| unsafe { write_sample(out, f.kind, f.big, k, v) };
        match f.colors {
            Colors::Gray => put(f.order[0], gray8([r, g, b, a])),
            Colors::Rgb => {
                put(f.order[0], r);
                put(f.order[1], g);
                put(f.order[2], b);
            }
            Colors::None => {}
        }
        match f.alpha {
            Some(k) => put(k, a),
            // RGB's fourth sample is padding, opaque as AppKit leaves it.
            None if f.colors == Colors::Rgb && f.slots == 4 => put(3, 255),
            None => {}
        }
    }
}

/// Sample `i` of the pixel at `px`, as 8 bits.
///
/// # Safety
///
/// The sample is inside readable memory.
pub(crate) unsafe fn read_sample(px: *const u8, kind: Sample, big: bool, i: usize) -> u8 {
    // SAFETY: as the caller promises.
    unsafe {
        match kind {
            Sample::U8 => *px.add(i),
            Sample::U16 => {
                let raw = px.cast::<u16>().add(i).read_unaligned();
                let v = if big { u16::from_be(raw) } else { u16::from_le(raw) };
                ((u32::from(v) * 255 + 32767) / 65535) as u8
            }
            Sample::F32 => {
                let raw = px.cast::<u32>().add(i).read_unaligned();
                let v = f32::from_bits(if big { u32::from_be(raw) } else { u32::from_le(raw) });
                (v.clamp(0.0, 1.0) * 255.0).round() as u8
            }
        }
    }
}

/// Store 8-bit `v` as sample `i` of the pixel at `px`.
///
/// # Safety
///
/// The sample is inside writable memory.
pub(crate) unsafe fn write_sample(px: *mut u8, kind: Sample, big: bool, i: usize, v: u8) {
    // SAFETY: as the caller promises.
    unsafe {
        match kind {
            Sample::U8 => *px.add(i) = v,
            Sample::U16 => {
                let v = u16::from(v) * 257;
                px.cast::<u16>().add(i).write_unaligned(if big { v.to_be() } else { v.to_le() });
            }
            Sample::F32 => {
                let bits = (f32::from(v) / 255.0).to_bits();
                px.cast::<u32>().add(i).write_unaligned(if big { bits.to_be() } else { bits.to_le() });
            }
        }
    }
}

/// A premultiplied canvas pixel's gray, premultiplied: its luminance in
/// linear light, encoded again (see `color::gray_of`).
pub(crate) fn gray8([r, g, b, a]: [u8; 4]) -> u8 {
    if r == g && g == b {
        return r;
    }
    if a == 0 {
        return 0;
    }
    let un = |c: u8| f64::from(c) / f64::from(a);
    let gray = crate::color::gray_of([un(r), un(g), un(b)]);
    (gray.clamp(0.0, 1.0) * f64::from(a)).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Color;

    const RED: Color = [1.0, 0.0, 0.0, 1.0];

    fn fill(rect: Rect, color: Color) -> Op {
        Op::Fill { rect, color }
    }

    #[test]
    fn packing_keeps_what_isnt_drawn() {
        // 16-bit gray and alpha (native order), 2 × 1: an odd value no
        // 8-bit round trip keeps, and a pixel drawn over.
        let f = Format {
            kind: Sample::U16,
            slots: 2,
            colors: Colors::Gray,
            order: [0; 3],
            alpha: Some(1),
            padding: false,
            big: cfg!(target_endian = "big"),
        };
        let mut px: Vec<u16> = vec![0x1234, 0xffff, 0x0101, 0xffff];
        let mem = Memory { base: px.as_mut_ptr().cast(), width: 2, height: 1, bpr: 8, format: f };
        let mut before = vec![0u32; 2];
        // SAFETY: `px` holds the one row.
        unsafe { unpack(&mem, (0, 0, 2, 1), &mut before) };
        assert_eq!(before[0].to_ne_bytes(), [0x12, 0x12, 0x12, 0xff]);
        let mut after = before.clone();
        after[1] = u32::from_ne_bytes([255, 255, 255, 255]);
        // SAFETY: as above.
        unsafe { pack(&mem, (0, 0, 2, 1), &after, &before) };
        assert_eq!(px[..4], [0x1234, 0xffff, 0xffff, 0xffff]);
        // Gray is luminance in linear light: red is a middle gray.
        assert!((122..=132).contains(&gray8([255, 0, 0, 255])), "{}", gray8([255, 0, 0, 255]));
        assert_eq!(gray8([77, 77, 77, 128]), 77);
    }

    #[test]
    fn byte_orders_and_regions() {
        // BGRA (alpha first, 32-bit little-endian): blue first in memory.
        let bgra = Format { order: [2, 1, 0], alpha: Some(3), ..Format::CANVAS };
        let mut px = vec![0u8; 4 * 4 * 2];
        let mem = Memory { base: px.as_mut_ptr(), width: 4, height: 2, bpr: 16, format: bgra };
        // SAFETY: `px` holds the rows.
        unsafe { rasterize(&mem, 1.0, &[fill(Rect::new(1.0, 1.0, 2.0, 2.0), RED)]) };
        assert_eq!(&px[16 + 4..16 + 8], &[0, 0, 255, 255]);
        assert!(px[..16].iter().all(|&b| b == 0), "only the pixel drawn");
        // Big-endian floats.
        let f = Format { kind: Sample::F32, big: true, ..Format::CANVAS };
        let mut px = vec![0u8; 16];
        let mem = Memory { base: px.as_mut_ptr(), width: 1, height: 1, bpr: 16, format: f };
        // SAFETY: as above.
        unsafe { rasterize(&mem, 1.0, &[fill(Rect::new(0.0, 0.0, 1.0, 1.0), RED)]) };
        assert_eq!(&px[..4], &1.0f32.to_be_bytes());
        assert_eq!(&px[4..8], &[0; 4]);
    }

    #[test]
    fn padding_keeps_its_colors() {
        // RGBX with a zero padding byte, as macOS blends it: the color
        // there blends with a half-transparent fill (not black), and the
        // padding takes the alpha.
        let rgbx = Format { padding: true, ..Format::CANVAS };
        let mut px = vec![0u8, 0, 255, 0];
        let mem = Memory { base: px.as_mut_ptr(), width: 1, height: 1, bpr: 4, format: rgbx };
        // SAFETY: `px` holds the pixel.
        unsafe { rasterize(&mem, 1.0, &[fill(Rect::new(0.0, 0.0, 1.0, 1.0), [1.0, 0.0, 0.0, 0.5])]) };
        assert_eq!(px, [128, 0, 127, 128]);
    }
}
