//! The render thread's side of text: each glyph is rasterized once per
//! face, pixel size and subpixel offset, then composited from the cache.
//!
//! Glyphs are placed at a quarter pixel horizontally, which keeps the
//! spacing shaping chose without blurring every glyph, and at whole pixels
//! vertically, where baselines sit. Outlines are anti-aliased coverage
//! masks; color glyphs (COLR layers, and CBDT or sbix bitmaps such as Noto
//! Color Emoji) are premultiplied color images. swash does the
//! rasterizing, without hinting, as macOS draws text. A face made only of
//! bitmaps can't be drawn between pixels, so its glyphs go to whole pixels
//! and are rasterized once per size.
//!
//! The cache keeps two generations of images, as the layout cache does: an
//! image drawn from the older one moves to the newer, and when the newer
//! one outgrows half the budget the older is dropped, so what the frames
//! being drawn use survives.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use swash::CacheKey;
use swash::scale::image::{Content, Image as Rendered};
use swash::scale::{Render, ScaleContext, Scaler, Source, StrikeWith};
use swash::zeno::{Angle, Format, Stroke, Transform, Vector};

use super::fonts::{self, FaceData};
use super::layout::FxBuild;
use crate::protocol::{Color, GlyphRun, Rect};
use crate::raster::Canvas;

/// Horizontal positions per pixel that a glyph is rasterized at.
const SUBPIXEL: f32 = 4.0;
/// Bytes of glyph images the cache keeps, both generations together.
const BUDGET: usize = 24 << 20;

/// Glyph images by face, size, glyph and subpixel offset.
pub(crate) struct Glyphs {
    context: ScaleContext,
    faces: HashMap<u32, Option<Face>, FxBuild>,
    /// Strikes by face and pixel size: the newer generation and the older.
    new: HashMap<(u32, u32), Strike, FxBuild>,
    old: HashMap<(u32, u32), Strike, FxBuild>,
    /// Bytes of images in the newer generation.
    bytes: usize,
}

impl Default for Glyphs {
    fn default() -> Self {
        Glyphs {
            context: ScaleContext::new(),
            faces: HashMap::default(),
            new: HashMap::default(),
            old: HashMap::default(),
            bytes: 0,
        }
    }
}

/// A registered face, ready to rasterize from.
struct Face {
    data: FaceData,
    /// Where the face's tables start in its file, and the key swash caches
    /// what it reads of them under: kept, so that swash parses each face
    /// once rather than for every glyph.
    offset: u32,
    key: CacheKey,
    /// Nothing but bitmaps (as Noto Color Emoji's CBDT): drawn at whole
    /// pixels, one image per glyph and size.
    bitmaps_only: bool,
}

impl Face {
    fn load(context: &mut ScaleContext, id: u32) -> Option<Face> {
        let data = fonts::face_data(id)?;
        let font = swash::FontRef::from_index(data.font.data.data(), data.font.index as usize)?;
        let (offset, key) = (font.offset, font.key);
        let scaler = context.builder(font).size(16.0).build();
        let bitmaps_only = !scaler.has_outlines()
            && !scaler.has_color_outlines()
            && (scaler.has_color_bitmaps() || scaler.has_bitmaps());
        Some(Face { data, offset, key, bitmaps_only })
    }

    fn scaler<'a>(&'a self, context: &'a mut ScaleContext, size: f32) -> Scaler<'a> {
        let font = swash::FontRef { data: self.data.font.data.data(), offset: self.offset, key: self.key };
        context.builder(font).size(size).hint(false).normalized_coords(self.data.coords.iter()).build()
    }
}

/// One face at one pixel size. Keys are glyph id × [`SUBPIXEL`] + offset;
/// `None` marks a glyph with nothing to draw.
#[derive(Default)]
struct Strike {
    glyphs: HashMap<u32, Option<Image>, FxBuild>,
}

struct Image {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    pixels: Pixels,
}

enum Pixels {
    /// Coverage, one byte per pixel.
    Mask(Box<[u8]>),
    /// Premultiplied ARGB.
    Color(Box<[u32]>),
}

/// Roughly what a cached image costs: its pixels and its entry.
fn cost(image: &Option<Image>) -> usize {
    match image {
        None => 16,
        Some(i) => {
            let pixels = i.width as usize * i.height as usize;
            48 + match i.pixels {
                Pixels::Mask(_) => pixels,
                Pixels::Color(_) => pixels * 4,
            }
        }
    }
}

/// Pixels per point on `canvas`. Points map to pixels as
/// `((x, y - origin_y) × scale)`.
fn scale_of(canvas: &Canvas) -> f32 {
    canvas.scale
}

/// Draw `run` into `canvas`, inside `clip` (layer coordinates).
pub(crate) fn draw_glyphs(canvas: &mut Canvas, cache: &mut Glyphs, run: &GlyphRun, clip: &Rect) {
    let scale = scale_of(canvas);
    let Some((cx0, cy0, cx1, cy1)) = pixel_clip(canvas, clip, scale) else { return };
    let size = run.size * scale;
    if !(size > 0.0 && size < 4096.0) {
        return;
    }
    let Glyphs { context, faces, new, old, bytes } = cache;
    if *bytes > BUDGET / 2 {
        *old = std::mem::take(new);
        *bytes = 0;
    }
    let face = faces.entry(run.font).or_insert_with(|| Face::load(context, run.font));
    let Some(face) = face.as_ref() else { return };
    let key = (run.font, size.to_bits());
    let strike = new.entry(key).or_default();
    let mut older = old.get_mut(&key);
    // The scaler is made on the run's first miss, and serves the rest.
    let mut context = Some(context);
    let mut scaler: Option<Scaler<'_>> = None;
    let (rgb, alpha) = channels(run.color);
    let (ox, oy) = (run.x * scale, (run.y - canvas.origin_y) * scale);
    // Glyphs rarely reach further than this from their origin.
    let reach = size * 2.0;
    for g in run.glyphs.iter() {
        let x = ox + g.x * scale;
        let y = (oy + g.y * scale).round();
        if x + reach < cx0 as f32 || x - reach > cx1 as f32 || y + reach < cy0 as f32 || y - reach > cy1 as f32 {
            continue;
        }
        let (px, bin) = if face.bitmaps_only {
            (x.round(), 0)
        } else {
            let px = x.floor();
            match ((x - px) * SUBPIXEL).round() as u32 {
                bin if bin as f32 >= SUBPIXEL => (px + 1.0, 0),
                bin => (px, bin),
            }
        };
        let glyph_key = g.id.wrapping_mul(SUBPIXEL as u32) + bin;
        let image = match strike.glyphs.entry(glyph_key) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let image = match older.as_mut().and_then(|s| s.glyphs.remove(&glyph_key)) {
                    Some(image) => image,
                    None => {
                        if scaler.is_none()
                            && let Some(context) = context.take()
                        {
                            scaler = Some(face.scaler(context, size));
                        }
                        scaler.as_mut().and_then(|s| render(s, &face.data, size, g.id, bin as f32 / SUBPIXEL))
                    }
                };
                *bytes += cost(&image);
                e.insert(image)
            }
        };
        if let Some(image) = image {
            blit(canvas, image, px as i32 + image.left, y as i32 - image.top, (cx0, cy0, cx1, cy1), rgb, alpha);
        }
    }
}

fn render(scaler: &mut Scaler<'_>, face: &FaceData, size: f32, id: u32, offset: f32) -> Option<Image> {
    let sources = [Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline];
    let mut render = Render::new(&sources);
    render.format(Format::Alpha).offset(Vector::new(offset, 0.0));
    if face.embolden {
        render.embolden((size / 24.0).clamp(0.25, 2.0));
    }
    if face.skew != 0.0 {
        render.transform(Some(Transform::skew(Angle::from_degrees(face.skew), Angle::ZERO)));
    }
    if face.stroke > 0.0 {
        render.style(Stroke::new(face.stroke * size));
    }
    let rendered = render.render(scaler, u16::try_from(id).ok()?)?;
    convert(rendered)
}

fn convert(r: Rendered) -> Option<Image> {
    let (width, height) = (r.placement.width, r.placement.height);
    if width == 0 || height == 0 {
        return None;
    }
    let count = width as usize * height as usize;
    let pixels = match r.content {
        Content::Mask => Pixels::Mask(r.data.get(..count)?.into()),
        Content::Color => {
            // Color layers come out premultiplied; embedded bitmaps don't.
            let straight = matches!(r.source, Source::ColorBitmap(_) | Source::Bitmap(_));
            let rgba = r.data.get(..count * 4)?;
            Pixels::Color(
                rgba.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| {
                        let a = u32::from(p[3]);
                        let c = |v: u8| if straight { (u32::from(v) * a + 127) / 255 } else { u32::from(v).min(a) };
                        (a << 24) | (c(p[0]) << 16) | (c(p[1]) << 8) | c(p[2])
                    })
                    .collect(),
            )
        }
        Content::SubpixelMask => return None,
    };
    Some(Image { left: r.placement.left, top: r.placement.top, width, height, pixels })
}

/// The pixels of `clip` on the canvas, as x0, y0, x1, y1: those whose
/// centers lie inside it, as for fills.
fn pixel_clip(canvas: &Canvas, clip: &Rect, scale: f32) -> Option<(i32, i32, i32, i32)> {
    let edge = |v: f32| (v * scale - 0.5).ceil();
    let x0 = edge(clip.x0).max(0.0);
    let y0 = edge(clip.y0 - canvas.origin_y).max(0.0);
    let x1 = edge(clip.x1).min(canvas.width as f32);
    let y1 = edge(clip.y1 - canvas.origin_y).min(canvas.height as f32);
    (x0 < x1 && y0 < y1).then_some((x0 as i32, y0 as i32, x1 as i32, y1 as i32))
}

fn channels(c: Color) -> ([u32; 3], u32) {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    ([ch(c[0]), ch(c[1]), ch(c[2])], ch(c[3]))
}

/// Composite `image` with its top left corner at (`x`, `y`).
fn blit(canvas: &mut Canvas, image: &Image, x: i32, y: i32, clip: (i32, i32, i32, i32), rgb: [u32; 3], alpha: u32) {
    let (cx0, cy0, cx1, cy1) = clip;
    let (w, h) = (image.width as i32, image.height as i32);
    let (c0, c1) = ((cx0 - x).max(0), (cx1 - x).min(w));
    let (r0, r1) = ((cy0 - y).max(0), (cy1 - y).min(h));
    if c0 >= c1 || r0 >= r1 {
        return;
    }
    let stride = canvas.width as usize;
    let len = (c1 - c0) as usize;
    let rows = (r0..r1).map(|row| ((y + row) as usize * stride + (x + c0) as usize, (row * w + c0) as usize));
    match &image.pixels {
        Pixels::Mask(mask) => {
            let solid = (rgb[0] << 16) | (rgb[1] << 8) | rgb[2];
            for (dst, src) in rows {
                for (d, &cov) in canvas.px[dst..dst + len].iter_mut().zip(&mask[src..src + len]) {
                    let a = if alpha == 255 { u32::from(cov) } else { (u32::from(cov) * alpha * 257 + 32896) >> 16 };
                    if a == 0 {
                        continue;
                    }
                    *d = if a >= 255 { solid } else { blend(*d, solid, a) };
                }
            }
        }
        Pixels::Color(color) => {
            for (dst, src) in rows {
                for (d, &s) in canvas.px[dst..dst + len].iter_mut().zip(&color[src..src + len]) {
                    *d = over(*d, s, alpha);
                }
            }
        }
    }
}

/// `solid` over `dst` with coverage `a` (below 255), rounded: red and blue
/// share a multiply, green has its own, and each lane's sum stays below
/// 2¹⁶, since `dst · (255 − a) + solid · a` is at most 255².
#[inline]
fn blend(dst: u32, solid: u32, a: u32) -> u32 {
    let inv = 255 - a;
    let rb = (dst & 0x00ff_00ff) * inv + (solid & 0x00ff_00ff) * a + 0x0080_0080;
    let g = (dst & 0x0000_ff00) * inv + (solid & 0x0000_ff00) * a + 0x0000_8000;
    let rb = ((rb + ((rb >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff;
    let g = ((g + ((g >> 8) & 0x0000_ff00)) >> 8) & 0x0000_ff00;
    rb | g
}

/// Premultiplied `src`, faded by `alpha`, over `dst`.
#[inline]
fn over(dst: u32, src: u32, alpha: u32) -> u32 {
    let fade = |v: u32| if alpha == 255 { v } else { (v * alpha + 127) / 255 };
    let sa = fade(src >> 24);
    if sa == 0 {
        return dst;
    }
    let inv = 255 - sa;
    let mix = |shift: u32| fade((src >> shift) & 0xff) + ((((dst >> shift) & 0xff) * inv) + 127) / 255;
    (mix(16).min(255) << 16) | (mix(8).min(255) << 8) | mix(0).min(255)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::protocol::Glyph;
    use crate::text::fonts::{Design, Family, FontSpec, resolve};

    /// The registered id of `family`'s regular face, if it's installed.
    fn face_id(family: &str) -> Option<u32> {
        let spec = FontSpec { family: Family::Named(family.into()), ..FontSpec::system(Design::Default, 0.0) };
        let face = resolve(&spec);
        let font = face.font.as_ref().filter(|_| &*face.family == family)?;
        Some(fonts::register(font, &[], fonts::Synth::default()))
    }

    fn face(context: &mut ScaleContext, family: &str) -> Option<Face> {
        Face::load(context, face_id(family)?)
    }

    fn glyph(face: &Face, c: char) -> u32 {
        let font = skrifa::FontRef::from_index(face.data.font.data.data(), face.data.font.index).unwrap();
        skrifa::MetadataProvider::charmap(&font).map(c).map_or(0, |g| g.to_u32())
    }

    fn raster(context: &mut ScaleContext, face: &Face, size: f32, id: u32, offset: f32) -> Option<Image> {
        render(&mut face.scaler(context, size), &face.data, size, id, offset)
    }

    #[test]
    fn outlines_are_masks_and_emoji_are_color() {
        let mut context = ScaleContext::new();
        let sans = face(&mut context, "DejaVu Sans").expect("DejaVu Sans is installed");
        assert!(!sans.bitmaps_only);
        let a = raster(&mut context, &sans, 20.0, glyph(&sans, 'a'), 0.0).expect("an image");
        assert!(matches!(a.pixels, Pixels::Mask(_)) && a.width > 4 && a.height > 4);
        let shifted = raster(&mut context, &sans, 20.0, glyph(&sans, 'a'), 0.5).unwrap();
        let (Pixels::Mask(m0), Pixels::Mask(m1)) = (&a.pixels, &shifted.pixels) else { unreachable!() };
        assert_ne!(m0, m1, "a subpixel offset moves the coverage");
        assert!(raster(&mut context, &sans, 20.0, glyph(&sans, ' '), 0.0).is_none(), "a space draws nothing");
        if let Some(emoji) = face(&mut context, "Noto Color Emoji") {
            let smile = raster(&mut context, &emoji, 32.0, glyph(&emoji, '😀'), 0.0).expect("an emoji");
            let Pixels::Color(px) = &smile.pixels else { panic!("emoji come in color") };
            assert!(px.iter().any(|p| (p >> 16) & 0xff != p & 0xff), "not gray");
            assert!((28..=40).contains(&smile.width), "scaled to the size asked ({})", smile.width);
        }
    }

    fn canvas(px: &mut [u32], width: u32, height: u32) -> Canvas<'_> {
        Canvas { px, width, height, origin_y: 0.0, scale: 1.0 }
    }

    fn run(font: u32, size: f32, glyphs: &[Glyph]) -> GlyphRun {
        GlyphRun {
            font,
            size,
            x: 0.0,
            y: size,
            glyphs: Arc::from(glyphs),
            color: [0.0, 0.0, 0.0, 1.0],
            clip: Rect::new(0.0, 0.0, 400.0, 100.0),
        }
    }

    fn images(cache: &Glyphs) -> usize {
        cache.new.values().chain(cache.old.values()).map(|s| s.glyphs.len()).sum()
    }

    #[test]
    fn bitmap_glyphs_are_rasterized_once_per_size() {
        let mut cache = Glyphs::default();
        let Some(id) = face_id("Noto Color Emoji") else { return };
        let emoji = Face::load(&mut cache.context, id).unwrap();
        assert!(emoji.bitmaps_only, "Noto Color Emoji is CBDT bitmaps");
        let g = glyph(&emoji, '😀');
        let mut px = vec![0u32; 400 * 100];
        let glyphs =
            [Glyph { id: g, x: 0.0, y: 0.0 }, Glyph { id: g, x: 40.3, y: 0.0 }, Glyph { id: g, x: 80.6, y: 0.0 }];
        let r = run(id, 32.0, &glyphs);
        draw_glyphs(&mut canvas(&mut px, 400, 100), &mut cache, &r, &r.clip);
        assert_eq!(images(&cache), 1, "one image serves every offset");
        assert!(px.iter().any(|&p| p != 0), "drawn");
        // An outline face keeps an image per quarter pixel.
        let Some(sans) = face_id("DejaVu Sans") else { return };
        let a = glyph(&Face::load(&mut cache.context, sans).unwrap(), 'a');
        let glyphs =
            [Glyph { id: a, x: 0.0, y: 0.0 }, Glyph { id: a, x: 10.25, y: 0.0 }, Glyph { id: a, x: 20.5, y: 0.0 }];
        let r = run(sans, 13.0, &glyphs);
        draw_glyphs(&mut canvas(&mut px, 400, 100), &mut cache, &r, &r.clip);
        assert_eq!(images(&cache), 4);
    }

    #[test]
    fn images_in_use_survive_a_turnover() {
        let mut cache = Glyphs::default();
        let Some(sans) = face_id("DejaVu Sans") else { return };
        let face = Face::load(&mut cache.context, sans).unwrap();
        let glyphs: Vec<Glyph> = "The quick brown fox"
            .chars()
            .enumerate()
            .map(|(i, c)| Glyph { id: glyph(&face, c), x: i as f32 * 12.0, y: 0.0 })
            .collect();
        let r = run(sans, 13.0, &glyphs);
        let mut px = vec![0u32; 400 * 100];
        draw_glyphs(&mut canvas(&mut px, 400, 100), &mut cache, &r, &r.clip);
        let first = images(&cache);
        assert!(cache.bytes > 0);
        // Past half the budget, the next run starts a new generation; what
        // it draws moves over and nothing is rasterized again.
        cache.bytes = BUDGET;
        draw_glyphs(&mut canvas(&mut px, 400, 100), &mut cache, &r, &r.clip);
        assert_eq!(cache.new.values().map(|s| s.glyphs.len()).sum::<usize>(), first);
        assert_eq!(cache.old.values().map(|s| s.glyphs.len()).sum::<usize>(), 0);
        // Another turnover drops what the older generation still holds.
        cache.bytes = BUDGET;
        let other = run(sans, 20.0, &glyphs[..3]);
        draw_glyphs(&mut canvas(&mut px, 400, 100), &mut cache, &other, &other.clip);
        assert_eq!(images(&cache), first + 3);
        cache.bytes = BUDGET;
        draw_glyphs(&mut canvas(&mut px, 400, 100), &mut cache, &other, &other.clip);
        assert_eq!(images(&cache), 3, "the 13-point images were in the generation dropped");
    }

    #[test]
    fn masks_cost_a_byte_a_pixel_and_colors_four() {
        let image = |pixels| Some(Image { left: 0, top: 0, width: 10, height: 10, pixels });
        assert_eq!(cost(&image(Pixels::Mask(vec![0; 100].into()))), 48 + 100);
        assert_eq!(cost(&image(Pixels::Color(vec![0; 100].into()))), 48 + 400);
    }

    #[test]
    fn blending_rounds_like_dividing() {
        let exact = |dst: u32, solid: u32, a: u32| {
            let mix = |shift: u32| ((((dst >> shift) & 0xff) * (255 - a) + ((solid >> shift) & 0xff) * a) + 127) / 255;
            (mix(16) << 16) | (mix(8) << 8) | mix(0)
        };
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..200_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let (dst, solid, a) = (seed as u32 & 0xff_ffff, (seed >> 24) as u32 & 0xff_ffff, (seed >> 56) as u32 % 255);
            assert_eq!(blend(dst, solid, a), exact(dst, solid, a), "{dst:06x} {solid:06x} {a}");
        }
    }

    #[test]
    #[ignore = "a benchmark; run in release mode"]
    fn bench_emoji_first_draw() {
        // Thirty distinct emoji at fractional pen positions, as running text
        // puts them, drawn into a cold cache: what a chat view's first frame
        // with them costs the render thread.
        let Some(id) = face_id("Noto Color Emoji") else { return };
        let mut context = ScaleContext::new();
        let face = Face::load(&mut context, id).unwrap();
        let emoji: Vec<u32> =
            "😀😁😂🤣😃😄😅😆😉😊😋😎😍😘🥰😗😙😚🙂🤗🤩🤔🤨😐😑😶🙄😏😣😥".chars().map(|c| glyph(&face, c)).collect();
        let mut px = vec![0u32; 400 * 100];
        // Bitmaps rasterized once per size, and (as before) once per offset.
        for (size, per_offset) in [(16.0, false), (16.0, true), (32.0, false), (32.0, true)] {
            let mut times: Vec<f64> = (0..7)
                .map(|_| {
                    let mut cache = Glyphs::default();
                    if per_offset {
                        let face = Face { bitmaps_only: false, ..Face::load(&mut cache.context, id).unwrap() };
                        cache.faces.insert(id, Some(face));
                    }
                    let glyphs: Vec<Glyph> = (0..4)
                        .flat_map(|line| {
                            emoji.iter().enumerate().map(move |(i, &g)| Glyph {
                                id: g,
                                x: i as f32 * 13.3 + line as f32 * 0.25,
                                y: 0.0,
                            })
                        })
                        .collect();
                    let r = GlyphRun { clip: Rect::new(0.0, 0.0, 400.0, 100.0), ..run(id, size, &glyphs) };
                    let start = std::time::Instant::now();
                    draw_glyphs(&mut canvas(&mut px, 400, 100), &mut cache, &r, &r.clip);
                    start.elapsed().as_secs_f64() * 1e3
                })
                .collect();
            times.sort_by(f64::total_cmp);
            let how = if per_offset { "an image per offset" } else { "an image per size" };
            println!("30 emoji at 4 offsets each, {size} px, cold cache, {how}: {:.2} ms", times[3]);
        }
    }

    #[test]
    #[ignore = "a benchmark; run in release mode"]
    fn bench_glyph_rasterization() {
        let mut context = ScaleContext::new();
        for family in ["DejaVu Sans", "Noto Sans CJK JP", "Noto Color Emoji"] {
            let Some(face) = face(&mut context, family) else { continue };
            let chars: Vec<char> = match family {
                "Noto Sans CJK JP" => "漢字仮名交じり文日本語中文한국어東京大阪京都".chars().collect(),
                "Noto Color Emoji" => "😀😁😂🤣😃😄😅😆😉😊😋😎😍😘🥰😗".chars().collect(),
                _ => (' '..='~').collect(),
            };
            let ids: Vec<u32> = chars.iter().map(|&c| glyph(&face, c)).collect();
            for size in [13.0, 26.0] {
                // A scaler per run of glyphs, as drawing makes them, and
                // (as before) a scaler, with the font parsed again, per glyph.
                let mut per_run = Vec::new();
                let mut per_glyph = Vec::new();
                for _ in 0..7 {
                    let start = std::time::Instant::now();
                    for round in 0..8 {
                        let mut scaler = face.scaler(&mut context, size);
                        for &id in &ids {
                            let offset = (round % 4) as f32 / SUBPIXEL;
                            std::hint::black_box(render(&mut scaler, &face.data, size, id, offset));
                        }
                    }
                    per_run.push(start.elapsed().as_secs_f64() * 1e6 / (8 * ids.len()) as f64);
                    let start = std::time::Instant::now();
                    for round in 0..8 {
                        for &id in &ids {
                            let data = &face.data.font;
                            let font = swash::FontRef::from_index(data.data.data(), data.index as usize).unwrap();
                            let mut scaler = context.builder(font).size(size).build();
                            let offset = (round % 4) as f32 / SUBPIXEL;
                            std::hint::black_box(render(&mut scaler, &face.data, size, id, offset));
                        }
                    }
                    per_glyph.push(start.elapsed().as_secs_f64() * 1e6 / (8 * ids.len()) as f64);
                }
                per_run.sort_by(f64::total_cmp);
                per_glyph.sort_by(f64::total_cmp);
                let (now, before) = (per_run[3], per_glyph[3]);
                println!(
                    "rasterize a {family} glyph at {size} px: {now:.2} µs ({:.0} glyphs/ms); a scaler per glyph: {before:.2} µs",
                    1e3 / now
                );
            }
        }
    }
}
