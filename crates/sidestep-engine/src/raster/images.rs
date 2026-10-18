//! Images on the rasterizing side: the pixels an image op names, and a
//! per-thread texture cache of them ready to draw.
//!
//! An [`ImageData`] is a snapshot of an image representation's pixels,
//! shared between threads through an `Arc`: premultiplied RGBA, or an
//! encoded file that the cache decodes the first time it's drawn, so
//! decoding happens off the main thread for windows. Its `key` names the
//! representation and `generation` counts changes to its pixels; the cache
//! keeps one entry per key and generation.
//!
//! An entry holds the image at full size, halvings of it made the first
//! time it's drawn smaller than half size (sampling a big image straight
//! down to a small one would skip most of its pixels), and tinted copies
//! for template images, the four most recently made. At full size a
//! snapshot's pixels are drawn where they are, shared with the snapshot
//! (they're already premultiplied RGBA, rows `width × 4` bytes apart); only
//! decoded files, halvings and tints have pixels of the cache's own.
//! Entries are dropped least recently used first once the cache outgrows
//! its budget (256 MiB unless `SIDESTEP_IMAGE_CACHE_MB` says otherwise),
//! and when their representation goes away (`forget`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use tiny_skia::{FilterQuality, Pixmap, PixmapRef, SpreadMode, Transform};

use super::Canvas;
use super::ops::{ScratchSource, region, with_region};
use crate::protocol::{Blend, Color, Draw, Quality, Rect};

/// An image's pixels as a draw sees them.
#[derive(Debug)]
pub struct ImageData {
    pub key: u64,
    pub generation: u64,
    /// Size in pixels (after orientation, for an encoded file).
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
}

#[derive(Debug)]
pub enum Pixels {
    /// Premultiplied RGBA, rows `width × 4` bytes apart, top row first.
    Rgba(Arc<[u8]>),
    /// A file to decode, turned upright by its orientation or not.
    Encoded(Arc<[u8]>, bool),
    /// Part of another image: its pixels from `x`, `y` (this image's
    /// size), cropped from the other's cache entry the first time it's
    /// drawn, so a file is decoded once however many parts are drawn, and
    /// where drawing happens.
    Part(Arc<ImageData>, u32, u32),
}

/// A cached image: its pixels, its halvings and its tinted copies.
struct Entry {
    generation: u64,
    base: Base,
    /// Whether every pixel at full size is opaque, once a blit asked.
    opaque: std::cell::Cell<Option<bool>>,
    /// Level 1 on: each half the size of the one before.
    halvings: Vec<Pixmap>,
    /// Tinted copies, oldest first, by tint and level.
    tints: Vec<((u32, u8), Pixmap)>,
    bytes: usize,
    used: u64,
}

/// An image at full size.
enum Base {
    /// A snapshot's premultiplied pixels, shared with it.
    Shared(Arc<[u8]>, u32, u32),
    /// Pixels decoded here.
    Owned(Pixmap),
}

/// The most tinted copies an entry keeps.
const TINTS: usize = 4;

impl Entry {
    /// Level 0 (full size) or a halving.
    fn level(&self, level: usize) -> PixmapRef<'_> {
        match (level, &self.base) {
            (0, Base::Shared(data, w, h)) => PixmapRef::from_bytes(data, *w, *h).expect("the snapshot's size"),
            (0, Base::Owned(p)) => p.as_ref(),
            (l, _) => self.halvings[l - 1].as_ref(),
        }
    }

    fn levels(&self) -> usize {
        1 + self.halvings.len()
    }
}

pub struct Cache {
    entries: HashMap<u64, Entry>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

impl Default for Cache {
    fn default() -> Self {
        let mb = std::env::var("SIDESTEP_IMAGE_CACHE_MB").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(256);
        Cache { entries: HashMap::new(), bytes: 0, budget: mb << 20, clock: 0 }
    }
}

thread_local!(static CACHE: RefCell<Cache> = RefCell::default());

static KEYS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A key naming a new image's pixels in the caches.
pub fn next_key() -> u64 {
    KEYS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Drop what this thread cached for the representations `keys`. (A
/// thread that is ending, its cache already gone, has nothing to drop.)
pub fn forget(keys: &[u64]) {
    let _ = CACHE.try_with(|c| {
        let Ok(mut c) = c.try_borrow_mut() else { return };
        for key in keys {
            if let Some(e) = c.entries.remove(key) {
                c.bytes -= e.bytes;
            }
        }
    });
}

fn cost(p: &Pixmap) -> usize {
    p.data().len() + 64
}

fn base_cost(b: &Base) -> usize {
    match b {
        // Shared, but kept alive by the cache all the same.
        Base::Shared(data, ..) => data.len() + 64,
        Base::Owned(p) => cost(p),
    }
}

impl Cache {
    /// The entry for `image`, made (decoding it) if needed.
    fn entry(&mut self, image: &ImageData) -> Option<&mut Entry> {
        self.clock += 1;
        let stale = self.entries.get(&image.key).is_some_and(|e| e.generation != image.generation);
        if stale && let Some(e) = self.entries.remove(&image.key) {
            self.bytes -= e.bytes;
        }
        if !self.entries.contains_key(&image.key) {
            let base = match &image.pixels {
                Pixels::Part(parent, x, y) => {
                    let whole = self.entry(parent)?.level(0);
                    crop(whole, *x, *y, image.width, image.height)?
                }
                _ => to_base(image)?,
            };
            let bytes = base_cost(&base);
            self.bytes += bytes;
            self.entries.insert(
                image.key,
                Entry {
                    generation: image.generation,
                    base,
                    opaque: Default::default(),
                    halvings: Vec::new(),
                    tints: Vec::new(),
                    bytes,
                    used: 0,
                },
            );
            self.evict(image.key);
        }
        let clock = self.clock;
        let e = self.entries.get_mut(&image.key)?;
        e.used = clock;
        Some(e)
    }

    /// Drop the least recently used entries but `keep` until the cache
    /// fits its budget.
    fn evict(&mut self, keep: u64) {
        while self.bytes > self.budget {
            let Some((&key, _)) = self.entries.iter().filter(|(k, _)| **k != keep).min_by_key(|(_, e)| e.used) else {
                return;
            };
            if let Some(e) = self.entries.remove(&key) {
                self.bytes -= e.bytes;
            }
        }
    }
}

/// `w` × `h` of `p` from `x`, `y`, as pixels of their own.
fn crop(p: PixmapRef, x: u32, y: u32, w: u32, h: u32) -> Option<Base> {
    if x.checked_add(w)? > p.width() || y.checked_add(h)? > p.height() {
        return None;
    }
    let mut out = Pixmap::new(w, h)?;
    let (sw, w) = (p.width() as usize * 4, w as usize * 4);
    for (row, dst) in out.data_mut().chunks_exact_mut(w).enumerate() {
        let at = (y as usize + row) * sw + x as usize * 4;
        dst.copy_from_slice(&p.data()[at..at + w]);
    }
    Some(Base::Owned(out))
}

fn to_base(image: &ImageData) -> Option<Base> {
    match &image.pixels {
        Pixels::Rgba(data) => {
            // Checked once here, so drawing can take the size as given.
            PixmapRef::from_bytes(data, image.width, image.height)?;
            Some(Base::Shared(data.clone(), image.width, image.height))
        }
        Pixels::Encoded(file, upright) => {
            let decoded = crate::codec::decode(file, *upright)?;
            let size = tiny_skia::IntSize::from_wh(decoded.width, decoded.height)?;
            Pixmap::from_vec(decoded.rgba, size).map(Base::Owned)
        }
        // Made from the cache (`Cache::entry`).
        Pixels::Part(..) => None,
    }
}

/// `p` at half its size, each pixel the average of four.
fn halve(p: PixmapRef) -> Option<Pixmap> {
    let (w, h) = ((p.width() / 2).max(1), (p.height() / 2).max(1));
    let mut out = Pixmap::new(w, h)?;
    let src = p.data();
    let sw = p.width() as usize;
    let (mw, mh) = (p.width() as usize - 1, p.height() as usize - 1);
    let dst = out.data_mut();
    for y in 0..h as usize {
        for x in 0..w as usize {
            for c in 0..4 {
                let at = |xx: usize, yy: usize| u32::from(src[(yy.min(mh) * sw + xx.min(mw)) * 4 + c]);
                let sum = at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1);
                dst[(y * w as usize + x) * 4 + c] = ((sum + 2) / 4) as u8;
            }
        }
    }
    Some(out)
}

/// `p`'s alpha filled with `tint`.
fn tinted(p: PixmapRef, tint: Color) -> Option<Pixmap> {
    let mut out = Pixmap::new(p.width(), p.height())?;
    for (d, s) in out.pixels_mut().iter_mut().zip(p.pixels()) {
        let a = f32::from(s.alpha()) / 255.0 * tint[3].clamp(0.0, 1.0);
        let ch = |v: f32| (v.clamp(0.0, 1.0) * a * 255.0 + 0.5) as u8;
        if let Some(px) =
            tiny_skia::PremultipliedColorU8::from_rgba(ch(tint[0]), ch(tint[1]), ch(tint[2]), (a * 255.0 + 0.5) as u8)
        {
            *d = px;
        }
    }
    Some(out)
}

fn filter(q: Quality) -> FilterQuality {
    match q {
        Quality::None => FilterQuality::Nearest,
        Quality::Low | Quality::Medium => FilterQuality::Bilinear,
        Quality::High => FilterQuality::Bicubic,
    }
}

/// The rectangle an image op fills, in its user space.
pub fn dst_bounds(dst: &Rect) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_ltrb(dst.x0.min(dst.x1), dst.y0.min(dst.y1), dst.x0.max(dst.x1), dst.y0.max(dst.y1))
}

/// Draw `src` (pixels, top-left origin) of `image` into `dst`, a
/// rectangle in the op's user space whose `y0` edge takes the image's top;
/// or, `tiled`, repeated from there over all of the clip.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    canvas: &mut Canvas,
    st: &mut dyn ScratchSource,
    damage: &Rect,
    image: &ImageData,
    (src, dst): (&Rect, &Rect),
    alpha: f32,
    quality: Quality,
    tint: Option<Color>,
    tiled: bool,
    draw: &Draw,
) {
    let (sw, sh) = (src.x1 - src.x0, src.y1 - src.y0);
    if sw <= 0.0 || sh <= 0.0 || alpha <= 0.0 {
        return;
    }
    let Some(bounds) = dst_bounds(dst) else { return };
    let region = if tiled { canvas.clip_pixels(draw, damage) } else { region(canvas, damage, draw, bounds, 1.0) };
    let Some(region) = region else { return };
    let base = canvas.transform().pre_concat(draw.xf);
    // Image pixels to user space.
    let placed = Transform::from_row((dst.x1 - dst.x0) / sw, 0.0, 0.0, (dst.y1 - dst.y0) / sh, dst.x0, dst.y0)
        .pre_translate(-src.x0, -src.y0);
    // How many device pixels an image pixel covers.
    let device = base.pre_concat(placed);
    let shrink = device.sx.hypot(device.ky).min(device.kx.hypot(device.sy));
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let Some(entry) = cache.entry(image) else { return };
        // Bytes this draw adds to the entry (halvings, a tint) and drops
        // from it (the oldest tint).
        let (mut added, mut dropped) = (0, 0);
        // A halving per octave below half size, for the filter to sample.
        let mut level = 0;
        if quality != Quality::None {
            while shrink * (1u32 << (level + 1)) as f32 <= 1.0 && level < 12 {
                level += 1;
            }
        }
        while entry.levels() <= level {
            let Some(next) = halve(entry.level(entry.levels() - 1)) else { break };
            added += cost(&next);
            entry.halvings.push(next);
        }
        let level = level.min(entry.levels() - 1);
        let tint_index = match tint {
            None => None,
            Some(t) => {
                let key = (super::premultiplied(t), level as u8);
                match entry.tints.iter().position(|(k, _)| *k == key) {
                    Some(i) => Some(i),
                    None => {
                        let Some(p) = tinted(entry.level(level), t) else { return };
                        if entry.tints.len() >= TINTS {
                            dropped += cost(&entry.tints.remove(0).1);
                        }
                        added += cost(&p);
                        entry.tints.push((key, p));
                        Some(entry.tints.len() - 1)
                    }
                }
            }
        };
        entry.bytes = entry.bytes + added - dropped;
        // Whether it may be a blit (see `blit`).
        let blit_candidate = !tiled
            && tint.is_none()
            && level == 0
            && draw.shadow.is_none()
            && draw.mask.is_none()
            && alpha >= 1.0
            && matches!(draw.blend, Blend::SourceOver | Blend::Copy);
        let (w0, h0) = (image.width as f32, image.height as f32);
        let pixmap = match tint_index {
            None => entry.level(level),
            Some(i) => entry.tints[i].1.as_ref(),
        };
        let to_level = Transform::from_scale(w0 / pixmap.width() as f32, h0 / pixmap.height() as f32);
        let pattern_xf = placed.pre_concat(to_level);
        // An image whose pixels land one to one on whole device pixels (a
        // view's drawing composited where it was drawn) samples each at its
        // center, where the filter takes that pixel alone: nearest
        // neighbour leaves the same pixels, for a fraction of the work.
        let quality =
            if quality != Quality::High && one_to_one(&base.pre_concat(pattern_xf)) { Quality::None } else { quality };
        // Whether a blit replaces what's under it: copying, or opaque over
        // it.
        let replaces = || {
            draw.blend == Blend::Copy || {
                let opaque = entry.opaque.get().unwrap_or_else(|| opaque(pixmap.data()));
                entry.opaque.set(Some(opaque));
                opaque
            }
        };
        if blit_candidate
            && !tests::slow()
            && blit(canvas, region, bounds, &base, &pattern_xf, pixmap, quality == Quality::None, &replaces)
        {
            tests::blitted(true);
            cache.bytes = cache.bytes + added - dropped;
            cache.evict(image.key);
            return;
        }
        tests::blitted(false);
        with_region(canvas, st, region, |pm, spare, masks, origin| {
            // The region's pixels, from the canvas's (the origin is on the
            // layer's grid, for masks).
            let xf = base.post_translate(-(region.0 as f32), -(region.1 as f32));
            let mask = masks.get(draw.mask.as_ref(), pm.width(), pm.height(), origin);
            if tiled {
                // The pattern repeated over the whole region, in its pixels.
                let whole = tiny_skia::Rect::from_xywh(0.0, 0.0, pm.width() as f32, pm.height() as f32);
                let paint = tiny_skia::Paint {
                    shader: tiny_skia::Pattern::new(
                        pixmap,
                        SpreadMode::Repeat,
                        filter(quality),
                        alpha.clamp(0.0, 1.0),
                        xf.pre_concat(pattern_xf),
                    ),
                    blend_mode: super::ops::blend_mode(draw.blend),
                    anti_alias: false,
                    ..Default::default()
                };
                if let Some(whole) = whole {
                    super::ops::masked(pm, spare, mask, draw.blend, |pm, mask| {
                        pm.fill_rect(whole, &paint, Transform::identity(), mask)
                    });
                }
                return;
            }
            let paint = |opacity: f32, blend| tiny_skia::Paint {
                shader: tiny_skia::Pattern::new(pixmap, SpreadMode::Pad, filter(quality), opacity, pattern_xf),
                blend_mode: blend,
                anti_alias: false,
                ..Default::default()
            };
            if let Some(shadow) = &draw.shadow {
                super::effects::shadow(pm, shadow, origin.2, mask, |target, moved| {
                    target.fill_rect(bounds, &paint(1.0, tiny_skia::BlendMode::SourceOver), xf.post_concat(moved), None)
                });
            }
            let blend = super::ops::blend_mode(draw.blend);
            let paint = paint(alpha.clamp(0.0, 1.0), blend);
            super::ops::masked(pm, spare, mask, draw.blend, |pm, mask| pm.fill_rect(bounds, &paint, xf, mask));
        });
        cache.bytes = cache.bytes + added - dropped;
        cache.evict(image.key);
    });
}

/// Draw `pixmap` placed by `pattern_xf` (its pixels to user space, then
/// through `base` to the canvas's) where `bounds` lands within `region`,
/// as the slow way's tiny-skia draws it nearest-neighbour, source over or
/// copying, but row by row. The slow way fills `bounds` in the region's
/// pixels, sampling the image at each pixel's center through the inverse
/// transform, clamped to the image's edges; this takes the same sums (in
/// the same floats) once per column and once per row, which it can when
/// the transform keeps the image upright (scaled any amount, mirrored or
/// not), `bounds` lands on whole pixels, and the filter is nearest
/// neighbour: asked for (`nearest`), or what tiny-skia takes for any
/// filter when the image isn't scaled at all. When it `replace`s what's
/// there (an opaque image, or the copy blend; asked once it's sure to
/// draw) pixels are copied, and a row sampling the same image row as the
/// one above copies that; otherwise each pixel is composited over what's
/// there with tiny-skia's arithmetic. A large one
/// is shared with the raster workers, in bands of rows. Whether it drew;
/// if not, nothing was touched.
#[allow(clippy::too_many_arguments)]
fn blit(
    canvas: &mut Canvas,
    region: (usize, usize, usize, usize),
    bounds: tiny_skia::Rect,
    base: &Transform,
    pattern_xf: &Transform,
    pixmap: PixmapRef,
    nearest: bool,
    replace: &dyn Fn() -> bool,
) -> bool {
    let (rw, rh) = (region.2 - region.0, region.3 - region.1);
    // tiny-skia draws into a pixmap this large in tiles, offsetting the
    // sums.
    if rw >= 8192 || rh >= 8192 {
        return false;
    }
    // What the slow way fills through, in the region's pixels, and the
    // pattern's transform as tiny-skia combines and inverts it.
    let xf = base.post_translate(-(region.0 as f32), -(region.1 as f32));
    let Some(inv) = pattern_xf.post_concat(xf).invert() else { return false };
    if !inv.is_finite() || inv.kx != 0.0 || inv.ky != 0.0 || !(nearest || (inv.sx == 1.0 && inv.sy == 1.0)) {
        return false;
    }
    let Some(dev) = bounds.transform(xf) else { return false };
    let whole = |v: f32| (v - v.round()).abs() < 1e-3;
    if ![dev.left(), dev.top(), dev.right(), dev.bottom()].into_iter().all(whole) {
        return false;
    }
    // The region's pixels filled.
    let r = |v: f32, limit: usize| (v.round().max(0.0) as usize).min(limit);
    let (x0, x1) = (r(dev.left(), rw), r(dev.right(), rw));
    let (y0, y1) = (r(dev.top(), rh), r(dev.bottom(), rh));
    if x0 >= x1 || y0 >= y1 {
        return true;
    }
    let replace = replace();
    // The image pixel each column and row of those samples: its center
    // through the inverse, clamped inside the image, truncated.
    let sample = |v: usize, scale: f32, offset: f32, limit: u32| {
        let last = f32::from_bits((limit as f32).to_bits() - 1);
        ((v as f32 + 0.5) * scale + offset).max(0.0).min(last) as usize
    };
    let w = pixmap.width() as usize;
    let xs: Vec<usize> = (x0..x1).map(|x| sample(x, inv.sx, inv.tx, pixmap.width())).collect();
    let ys: Vec<usize> = (y0..y1).map(|y| sample(y, inv.sy, inv.ty, pixmap.height())).collect();
    // One to one across: each row a run of an image row's pixels.
    let run = xs.windows(2).all(|p| p[1] == p[0] + 1);
    let src = pixmap.data();
    // The `j`th row filled, into `dst`.
    let draw_row = |j: usize, dst: &mut [u32]| {
        let line = &src[ys[j] * w * 4..][..w * 4];
        if run {
            let from = &line[xs[0] * 4..(xs[xs.len() - 1] + 1) * 4];
            if replace {
                super::as_bytes(dst).copy_from_slice(from);
            } else {
                for (d, s) in dst.iter_mut().zip(from.as_chunks::<4>().0) {
                    *d = over(*d, *s);
                }
            }
        } else {
            for (d, &x) in dst.iter_mut().zip(&xs) {
                let s = [line[x * 4], line[x * 4 + 1], line[x * 4 + 2], line[x * 4 + 3]];
                *d = if replace { u32::from_ne_bytes(s) } else { over(*d, s) };
            }
        }
    };
    // The canvas's rows, from the first filled.
    let (cx0, cx1, cy0) = (region.0 + x0, region.0 + x1, region.1 + y0);
    let stride = canvas.stride;
    let end = ((cy0 + ys.len() - 1) * stride + cx1).min(canvas.px.len());
    let rows = &mut canvas.px[cy0 * stride..end];
    // Rows `first..`, in `rows`.
    let band = |first: usize, rows: &mut [u32]| {
        for i in 0..rows.len().div_ceil(stride) {
            let j = first + i;
            if replace && i > 0 && ys[j] == ys[j - 1] {
                let (above, here) = rows.split_at_mut(i * stride);
                here[cx0..cx1].copy_from_slice(&above[(i - 1) * stride + cx0..(i - 1) * stride + cx1]);
            } else {
                draw_row(j, &mut rows[i * stride + cx0..i * stride + cx1]);
            }
        }
    };
    let n = if xs.len() * ys.len() >= super::bands::BANDED { super::bands::workers() + 1 } else { 1 };
    let mut bands: Vec<(usize, &mut [u32])> = Vec::with_capacity(n);
    let mut rest = rows;
    for b in 0..n {
        let (from, to) = (ys.len() * b / n, ys.len() * (b + 1) / n);
        let len = if b + 1 == n { rest.len() } else { (to - from) * stride };
        let (this, after) = rest.split_at_mut(len);
        rest = after;
        bands.push((from, this));
    }
    let mut bands = bands.into_iter();
    let (first, mine) = bands.next().expect("a band");
    let band = &band;
    let others: Vec<Box<dyn FnOnce() + Send + '_>> =
        bands.map(|(from, rows)| -> Box<dyn FnOnce() + Send + '_> { Box::new(move || band(from, rows)) }).collect();
    super::bands::scoped(others, || band(first, mine));
    true
}

/// Whether every pixel of `data` (RGBA) is opaque: their alphas and'ed
/// together a word at a time, a stretch at a time.
fn opaque(data: &[u8]) -> bool {
    data.as_chunks::<4>().0.chunks(1 << 14).all(|stretch| {
        let all = stretch.iter().fold(u32::MAX, |all, p| all & u32::from_ne_bytes(*p));
        all.to_ne_bytes()[3] == 255
    })
}

/// `src` (premultiplied RGBA) over the canvas pixel `dst`, as tiny-skia's
/// pipeline composites an image's pixels, but in integers. The pipeline
/// takes each channel as `d × (1 − sa) + s` in floats (the 8-bit values
/// over 255), times 255, rounded. Exactly, that's `s + d × (255 − sa) /
/// 255`: a whole number of 255ths, never nearer a half than half of one
/// (0.002), and the floats are off by a ten-thousandth at most, so they
/// round as the exact sum does (`over_is_the_pipelines` holds every value
/// to them).
fn over(dst: u32, src: [u8; 4]) -> u32 {
    let (d, inv) = (dst.to_ne_bytes(), 255 - u32::from(src[3]));
    let ch = |s: u8, d: u8| (u32::from(s) + (u32::from(d) * inv + 127) / 255).min(255) as u8;
    u32::from_ne_bytes([ch(src[0], d[0]), ch(src[1], d[1]), ch(src[2], d[2]), ch(src[3], d[3])])
}

/// Whether `m` takes image pixels to device pixels one to one: a whole
/// number of pixels across and down, mirrored or not, without scaling.
fn one_to_one(m: &Transform) -> bool {
    let whole = |v: f32| (v - v.round()).abs() < 1e-3;
    (m.sx.abs() - 1.0).abs() < 1e-5
        && (m.sy.abs() - 1.0).abs() < 1e-5
        && m.kx.abs() < 1e-5
        && m.ky.abs() < 1e-5
        && whole(m.tx)
        && whole(m.ty)
}

#[cfg(not(test))]
mod tests {
    /// Whether a test draws every image the slow way, to hold blits to it.
    pub fn slow() -> bool {
        false
    }

    pub fn blitted(_: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local!(static SLOW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

    pub fn slow() -> bool {
        SLOW.with(std::cell::Cell::get)
    }

    thread_local!(static BLITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });

    pub fn blitted(blit: bool) {
        BLITS.with(|b| b.set(b.get() + usize::from(blit)));
    }
    use crate::protocol::Blend;
    use crate::raster::channels;

    fn image(key: u64, w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> ImageData {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&f(x, y));
            }
        }
        ImageData { key, generation: 0, width: w, height: h, pixels: Pixels::Rgba(data.into()) }
    }

    fn draw_into(px: &mut [u32], w: u32, h: u32, img: &ImageData, src: Rect, dst: Rect, tint: Option<Color>) {
        let mut canvas = Canvas::new(px, w, h, 0.0, 1.0);
        let d = Draw {
            xf: Transform::identity(),
            blend: Blend::SourceOver,
            aa: true,
            clip: Rect::new(0.0, 0.0, w as f32, h as f32),
            mask: None,
            shadow: None,
        };
        let op = crate::protocol::Op::Image {
            image: Arc::new(ImageData {
                key: img.key,
                generation: img.generation,
                width: img.width,
                height: img.height,
                pixels: match &img.pixels {
                    Pixels::Rgba(d) => Pixels::Rgba(d.clone()),
                    Pixels::Encoded(d, u) => Pixels::Encoded(d.clone(), *u),
                    Pixels::Part(p, x, y) => Pixels::Part(p.clone(), *x, *y),
                },
            }),
            src,
            dst,
            alpha: 1.0,
            quality: Quality::None,
            tint,
            tiled: false,
            draw: d,
        };
        crate::raster::paint(
            &mut canvas,
            &mut crate::raster::Glyphs::default(),
            &[Rect::new(0.0, 0.0, w as f32, h as f32)],
            &[op],
        );
    }

    /// Draw `img` (`src`, image pixels) into `dst` (user space) of a canvas
    /// `w` × `h` pixels at `scale`, its pixels first `ground`, through
    /// `damage` and `clip`, as a blit where it can be, or the slow way.
    #[allow(clippy::too_many_arguments)]
    fn drawn(
        slow: bool,
        (w, h, scale): (u32, u32, f32),
        ground: &dyn Fn(u32, u32) -> u32,
        img: &Arc<ImageData>,
        (src, dst): (Rect, Rect),
        (damage, clip): (Rect, Rect),
        (quality, blend, xf): (Quality, Blend, Transform),
    ) -> Vec<u32> {
        let mut px: Vec<u32> = (0..w * h).map(|i| ground(i % w, i / w)).collect();
        let mut canvas = Canvas::new(&mut px, w, h, 0.0, scale);
        let draw = Draw { xf, blend, aa: true, clip, mask: None, shadow: None };
        let op = crate::protocol::Op::Image {
            image: img.clone(),
            src,
            dst,
            alpha: 1.0,
            quality,
            tint: None,
            tiled: false,
            draw,
        };
        SLOW.with(|s| s.set(slow));
        crate::raster::paint(&mut canvas, &mut crate::raster::Glyphs::default(), &[damage], &[op]);
        SLOW.with(|s| s.set(false));
        px
    }

    /// Images landing on whole device pixels, upright, one to one or
    /// nearest-neighbour, are blitted: copied, or composited row by row,
    /// enlarged or shrunk any amount, mirrored, padded past their edges,
    /// in bands on the workers when large; every pixel is the one the slow
    /// way draws, translucent ones over every ground included. Those it
    /// can't blit (a fraction of a pixel off, smoothed while scaled,
    /// turned) are drawn the slow way.
    #[test]
    fn blits_draw_what_the_slow_way_draws() {
        let premultiply = |c: u8, a: u8| ((u32::from(c) * u32::from(a) + 127) / 255) as u8;
        let opaque =
            Arc::new(image(40, 640, 480, |x, y| [(x % 256) as u8, (y % 256) as u8, ((x * y) % 256) as u8, 255]));
        // Every alpha down the rows, every color across them.
        let translucent = Arc::new(image(41, 256, 256, |x, y| {
            let (a, c) = (y as u8, x as u8);
            [premultiply(c, a), premultiply(255 - c, a), premultiply(c / 2, a), a]
        }));
        let grounds: [&dyn Fn(u32, u32) -> u32; 3] = [
            &|x, y| u32::from_ne_bytes([(x * 7 % 256) as u8, (y * 3 % 256) as u8, ((x + y) % 256) as u8, 255]),
            &|x, y| u32::from_ne_bytes([(x % 128) as u8, 0, (y % 64) as u8, ((x % 128) * 2) as u8]),
            &|_, _| 0,
        ];
        let r = Rect::new;
        let up = Transform::identity();
        // Turned a quarter, about (350, 0).
        let turned = Transform::from_row(0.0, 1.0, -1.0, 0.0, 350.0, 0.0);
        let (none, low, medium, high) = (Quality::None, Quality::Low, Quality::Medium, Quality::High);
        let (over, copy) = (Blend::SourceOver, Blend::Copy);
        #[rustfmt::skip]
        // The image, the scale, src, dst, how it's drawn, and whether that
        // blits.
        type Case<'a> = (&'a Arc<ImageData>, f32, Rect, Rect, (Quality, Blend, Transform), bool);
        let cases: Vec<Case> = vec![
            // One to one at whole pixels, at scales 1 and 2.
            (&opaque, 1.0, r(0.0, 0.0, 640.0, 480.0), r(3.0, 5.0, 643.0, 485.0), (none, over, up), true),
            (&opaque, 2.0, r(0.0, 0.0, 640.0, 480.0), r(1.5, 2.0, 321.5, 242.0), (medium, copy, up), true),
            (&opaque, 1.0, r(10.0, 20.0, 300.0, 200.0), r(0.0, 0.0, 290.0, 180.0), (low, over, up), true),
            (&translucent, 1.0, r(0.0, 0.0, 256.0, 256.0), r(7.0, 9.0, 263.0, 265.0), (none, over, up), true),
            (&translucent, 2.0, r(0.0, 0.0, 256.0, 256.0), r(4.0, 4.5, 132.0, 132.5), (none, copy, up), true),
            // Smoothed, but not scaled: tiny-skia takes the nearest pixel.
            (&opaque, 1.0, r(0.0, 0.0, 640.0, 480.0), r(0.0, 0.0, 640.0, 480.0), (high, over, up), true),
            (&translucent, 2.0, r(0.0, 0.0, 256.0, 256.0), r(20.0, 10.0, 148.0, 138.0), (high, over, up), true),
            // Enlarged two, three and one and a half times, nearest.
            (&translucent, 1.0, r(0.0, 0.0, 256.0, 256.0), r(1.0, 2.0, 513.0, 514.0), (none, over, up), true),
            (&opaque, 1.0, r(0.0, 0.0, 200.0, 150.0), r(0.0, 0.0, 600.0, 450.0), (none, over, up), true),
            (&opaque, 2.0, r(10.0, 10.0, 210.0, 160.0), r(0.0, 0.0, 150.0, 112.5), (none, over, up), true),
            // Shrunk by halves, thirds and 0.7, opaque and not.
            (&opaque, 1.0, r(0.0, 0.0, 640.0, 480.0), r(0.0, 0.0, 320.0, 240.0), (none, copy, up), true),
            (&opaque, 1.0, r(0.0, 0.0, 639.0, 480.0), r(5.0, 5.0, 218.0, 165.0), (none, over, up), true),
            (&opaque, 1.0, r(0.0, 0.0, 640.0, 480.0), r(0.0, 0.0, 448.0, 336.0), (none, over, up), true),
            (&translucent, 1.0, r(0.0, 0.0, 256.0, 256.0), r(30.0, 40.0, 230.0, 220.0), (none, over, up), true),
            // Mirrored both ways; padded past the image's edges.
            (&translucent, 1.0, r(0.0, 0.0, 256.0, 256.0), r(300.0, 280.0, 44.0, 24.0), (none, over, up), true),
            (&opaque, 1.0, r(-20.0, -10.0, 620.0, 470.0), r(0.0, 0.0, 640.0, 480.0), (none, over, up), true),
            // A quarter pixel off, smoothed while scaled, and turned: the
            // slow way.
            (&opaque, 1.0, r(0.0, 0.0, 640.0, 480.0), r(0.25, 0.0, 640.25, 480.0), (none, over, up), false),
            (&opaque, 1.0, r(0.0, 0.0, 200.0, 150.0), r(0.0, 0.0, 400.0, 300.0), (high, over, up), false),
            (&opaque, 1.0, r(0.0, 0.0, 200.0, 150.0), r(0.0, 0.0, 300.0, 225.0), (medium, over, up), false),
            (&translucent, 1.0, r(0.0, 0.0, 256.0, 256.0), r(0.0, 0.0, 256.0, 256.0), (none, over, turned), false),
        ];
        let size = (700, 560);
        for (n, (img, scale, src, dst, how, blits)) in cases.into_iter().enumerate() {
            let before = BLITS.with(std::cell::Cell::get);
            let canvas = (size.0, size.1, scale);
            let points = (size.0 as f32 / scale, size.1 as f32 / scale);
            // The whole canvas, and damage and a clip cutting through it.
            let whole = Rect::new(0.0, 0.0, points.0, points.1);
            let cut = (
                Rect::new(10.0, 33.5, points.0 - 20.0, points.1 - 7.0),
                Rect::new(0.0, 2.0, points.0 - 31.0, points.1),
            );
            for (damage, clip) in [(whole, whole), cut] {
                for ground in grounds {
                    let fast = drawn(false, canvas, ground, img, (src, dst), (damage, clip), how);
                    let slow = drawn(true, canvas, ground, img, (src, dst), (damage, clip), how);
                    let differ = fast.iter().zip(&slow).filter(|(a, b)| a != b).count();
                    assert_eq!(differ, 0, "case {n}: {differ} pixels differ: {dst:?} at {scale}, {how:?}, {damage:?}");
                }
            }
            let drew = BLITS.with(std::cell::Cell::get) - before;
            assert_eq!(drew > 0, blits, "case {n}: {drew} blits");
        }
    }

    /// [`over`] composites every channel value over every other at every
    /// alpha as tiny-skia's pipeline does: `d × (1 − sa) + s` in floats,
    /// clamped, times 255, rounded to even.
    #[test]
    fn over_is_the_pipelines() {
        const UNIT: f32 = 1.0 / 255.0;
        for sa in 0..=255u8 {
            let inv = 1.0 - f32::from(sa) * UNIT;
            for s in 0..=255u8 {
                for d in 0..=255u8 {
                    let float = ((f32::from(d) * UNIT * inv + f32::from(s) * UNIT).clamp(0.0, 1.0) * 255.0)
                        .round_ties_even() as u8;
                    let got = over(u32::from_ne_bytes([d, d, d, d]), [s, s, s, sa]).to_ne_bytes()[0];
                    assert_eq!(got, float, "{s} over {d} at alpha {sa}");
                }
            }
        }
    }

    #[test]
    fn images_scale_into_their_rectangle_top_row_first() {
        // Top half red, bottom half blue, 2 × 2, drawn 8 × 8.
        let img = image(1, 2, 2, |_, y| if y == 0 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        let mut px = vec![0u32; 100];
        draw_into(&mut px, 10, 10, &img, Rect::new(0.0, 0.0, 2.0, 2.0), Rect::new(1.0, 1.0, 9.0, 9.0), None);
        assert_eq!(channels(px[10 + 1]), [255, 0, 0, 255]);
        assert_eq!(channels(px[8 * 10 + 8]), [0, 0, 255, 255]);
        assert_eq!(channels(px[0]), [0; 4]);
        // Upside down: dst's y0 edge takes the top row wherever it is.
        let mut px = vec![0u32; 100];
        draw_into(&mut px, 10, 10, &img, Rect::new(0.0, 0.0, 2.0, 2.0), Rect::new(1.0, 9.0, 9.0, 1.0), None);
        assert_eq!(channels(px[10 + 1]), [0, 0, 255, 255]);
    }

    #[test]
    fn templates_draw_in_their_tint() {
        let img = image(2, 2, 2, |x, _| if x == 0 { [0, 0, 0, 255] } else { [0, 0, 0, 0] });
        let mut px = vec![0u32; 16];
        draw_into(
            &mut px,
            4,
            4,
            &img,
            Rect::new(0.0, 0.0, 2.0, 2.0),
            Rect::new(0.0, 0.0, 4.0, 4.0),
            Some([0.0, 1.0, 0.0, 1.0]),
        );
        assert_eq!(channels(px[0]), [0, 255, 0, 255]);
        assert_eq!(channels(px[3]), [0; 4]);
    }

    #[test]
    fn small_draws_use_halvings() {
        // A fine checkerboard, drawn at a sixteenth: gray, not aliased
        // black or white.
        let img = image(3, 64, 64, |x, y| if (x + y) % 2 == 0 { [255; 4] } else { [0, 0, 0, 255] });
        let mut px = vec![0u32; 16];
        let mut canvas = Canvas::new(&mut px, 4, 4, 0.0, 1.0);
        let d = Draw {
            xf: Transform::identity(),
            blend: Blend::SourceOver,
            aa: true,
            clip: Rect::new(0.0, 0.0, 4.0, 4.0),
            mask: None,
            shadow: None,
        };
        let op = crate::protocol::Op::Image {
            image: Arc::new(img),
            src: Rect::new(0.0, 0.0, 64.0, 64.0),
            dst: Rect::new(0.0, 0.0, 4.0, 4.0),
            alpha: 1.0,
            quality: Quality::Medium,
            tint: None,
            tiled: false,
            draw: d,
        };
        crate::raster::paint(
            &mut canvas,
            &mut crate::raster::Glyphs::default(),
            &[Rect::new(0.0, 0.0, 4.0, 4.0)],
            &[op],
        );
        let [r, ..] = channels(px[5]);
        assert!((100..160).contains(&r), "{r}");
        CACHE.with(|c| {
            let c = c.borrow();
            assert!(c.entries[&3].levels() >= 4, "halved down to 8 × 8");
            assert_eq!(c.bytes, c.entries.values().map(|e| e.bytes).sum::<usize>(), "halvings counted");
        });
        forget(&[3]);
    }

    #[test]
    fn tints_are_kept_apart_and_few() {
        // Black at two alphas on two levels once collided (the level was
        // mixed into the color's bits).
        let img = image(4, 64, 64, |_, _| [0, 0, 0, 255]);
        let draw_tinted = |tint: Color, size: f32| {
            let mut px = vec![0u32; 64 * 64];
            let mut canvas = Canvas::new(&mut px, 64, 64, 0.0, 1.0);
            let op = crate::protocol::Op::Image {
                image: Arc::new(ImageData {
                    key: img.key,
                    generation: 0,
                    width: 64,
                    height: 64,
                    pixels: match &img.pixels {
                        Pixels::Rgba(d) => Pixels::Rgba(d.clone()),
                        Pixels::Encoded(d, u) => Pixels::Encoded(d.clone(), *u),
                        Pixels::Part(p, x, y) => Pixels::Part(p.clone(), *x, *y),
                    },
                }),
                src: Rect::new(0.0, 0.0, 64.0, 64.0),
                dst: Rect::new(0.0, 0.0, size, size),
                alpha: 1.0,
                quality: Quality::Medium,
                tint: Some(tint),
                tiled: false,
                draw: Draw {
                    xf: Transform::identity(),
                    blend: Blend::Copy,
                    aa: true,
                    clip: Rect::new(0.0, 0.0, 64.0, 64.0),
                    mask: None,
                    shadow: None,
                },
            };
            crate::raster::paint(
                &mut canvas,
                &mut crate::raster::Glyphs::default(),
                &[Rect::new(0.0, 0.0, 64.0, 64.0)],
                &[op],
            );
            channels(px[0])[3]
        };
        let (a, b) = (0xd9 as f32 / 255.0, 0xc9 as f32 / 255.0);
        assert_eq!(draw_tinted([0.0, 0.0, 0.0, a], 64.0), 0xd9);
        assert_eq!(draw_tinted([0.0, 0.0, 0.0, b], 32.0), 0xc9, "another tint on another level");
        for i in 0..10 {
            draw_tinted([i as f32 / 10.0, 0.0, 0.0, 1.0], 64.0);
        }
        CACHE.with(|c| {
            let c = c.borrow();
            assert!(c.entries[&4].tints.len() <= TINTS);
            assert_eq!(c.bytes, c.entries.values().map(|e| e.bytes).sum::<usize>(), "dropped tints uncounted");
        });
        forget(&[4]);
    }

    #[test]
    fn the_cache_keeps_to_its_budget() {
        let mut cache = Cache { budget: 3 * (16 * 16 * 4 + 64), ..Cache::default() };
        for key in 0..5 {
            cache.entry(&image(100 + key, 16, 16, |_, _| [0; 4])).expect("entry");
        }
        assert_eq!(cache.entries.len(), 3, "the oldest went");
        assert!(cache.entries.contains_key(&104) && !cache.entries.contains_key(&100));
        // Using one keeps it.
        cache.entry(&image(102, 16, 16, |_, _| [0; 4])).unwrap();
        cache.entry(&image(105, 16, 16, |_, _| [0; 4])).unwrap();
        assert!(cache.entries.contains_key(&102) && !cache.entries.contains_key(&103));
        // A new generation replaces the old.
        let mut changed = image(102, 16, 16, |_, _| [255; 4]);
        changed.generation = 1;
        cache.entry(&changed).unwrap();
        assert_eq!(cache.entries[&102].generation, 1);
        assert!(cache.bytes <= cache.budget);
    }
}
