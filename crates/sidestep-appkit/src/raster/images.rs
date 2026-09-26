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
use crate::protocol::{Color, Draw, Quality, Rect};

/// An image's pixels as a draw sees them.
#[derive(Debug)]
pub(crate) struct ImageData {
    pub key: u64,
    pub generation: u64,
    /// Size in pixels (after orientation, for an encoded file).
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
}

#[derive(Debug)]
pub(crate) enum Pixels {
    /// Premultiplied RGBA, rows `width × 4` bytes apart, top row first.
    Rgba(Arc<[u8]>),
    /// A file to decode, turned upright by its orientation or not.
    Encoded(Arc<[u8]>, bool),
}

/// A cached image: its pixels, its halvings and its tinted copies.
struct Entry {
    generation: u64,
    base: Base,
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

pub(crate) struct Cache {
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

/// Drop what this thread cached for the representations `keys`. (A
/// thread that is ending, its cache already gone, has nothing to drop.)
pub(crate) fn forget(keys: &[u64]) {
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
            let base = to_base(image)?;
            let bytes = base_cost(&base);
            self.bytes += bytes;
            self.entries.insert(
                image.key,
                Entry { generation: image.generation, base, halvings: Vec::new(), tints: Vec::new(), bytes, used: 0 },
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
pub(crate) fn dst_bounds(dst: &Rect) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_ltrb(dst.x0.min(dst.x1), dst.y0.min(dst.y1), dst.x0.max(dst.x1), dst.y0.max(dst.y1))
}

/// Draw `src` (pixels, top-left origin) of `image` into `dst`, a
/// rectangle in the op's user space whose `y0` edge takes the image's top.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    canvas: &mut Canvas,
    st: &mut dyn ScratchSource,
    damage: &Rect,
    image: &ImageData,
    (src, dst): (&Rect, &Rect),
    alpha: f32,
    quality: Quality,
    tint: Option<Color>,
    draw: &Draw,
) {
    let (sw, sh) = (src.x1 - src.x0, src.y1 - src.y0);
    if sw <= 0.0 || sh <= 0.0 || alpha <= 0.0 {
        return;
    }
    let Some(bounds) = dst_bounds(dst) else { return };
    let Some(region) = region(canvas, damage, draw, bounds, 1.0) else { return };
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
        let (w0, h0) = (image.width as f32, image.height as f32);
        let pixmap = match tint_index {
            None => entry.level(level),
            Some(i) => entry.tints[i].1.as_ref(),
        };
        let to_level = Transform::from_scale(w0 / pixmap.width() as f32, h0 / pixmap.height() as f32);
        let pattern_xf = placed.pre_concat(to_level);
        with_region(canvas, st, region, |pm, spare, masks, origin| {
            // The region's pixels, from the canvas's (the origin is on the
            // layer's grid, for masks).
            let xf = base.post_translate(-(region.0 as f32), -(region.1 as f32));
            let mask = masks.get(draw.mask.as_ref(), pm.width(), pm.height(), origin);
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

#[cfg(test)]
mod tests {
    use super::*;
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
                },
            }),
            src,
            dst,
            alpha: 1.0,
            quality: Quality::None,
            tint,
            draw: d,
        };
        crate::raster::paint(
            &mut canvas,
            &mut crate::raster::Glyphs::default(),
            &[Rect::new(0.0, 0.0, w as f32, h as f32)],
            &[op],
        );
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
                    },
                }),
                src: Rect::new(0.0, 0.0, 64.0, 64.0),
                dst: Rect::new(0.0, 0.0, size, size),
                alpha: 1.0,
                quality: Quality::Medium,
                tint: Some(tint),
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
