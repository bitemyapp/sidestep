//! The render thread's side of text: each glyph is rasterized once per
//! face, pixel size and subpixel offset, then composited from the cache.
//!
//! Glyphs are placed at a quarter pixel horizontally, which keeps the
//! spacing shaping chose without blurring every glyph, and at whole pixels
//! vertically, where baselines sit. Outlines are anti-aliased coverage
//! masks; color glyphs (COLR layers, and CBDT or sbix bitmaps such as Noto
//! Color Emoji) are premultiplied color images. swash does the
//! rasterizing, without hinting, as macOS draws text.

use std::collections::HashMap;

use swash::scale::image::{Content, Image as Rendered};
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Transform, Vector};

use super::fonts::{self, FaceData};
use super::layout::FxBuild;
use crate::protocol::{Color, GlyphRun, Rect};
use crate::raster::Canvas;

/// Horizontal positions per pixel that a glyph is rasterized at.
const SUBPIXEL: f32 = 4.0;
/// When cached glyph images pass this many bytes, the cache starts over.
const BUDGET: usize = 24 << 20;

/// Glyph images by face, size, glyph and subpixel offset.
pub(crate) struct Glyphs {
    context: ScaleContext,
    faces: HashMap<u32, Option<FaceData>, FxBuild>,
    strikes: HashMap<(u32, u32), Strike, FxBuild>,
    bytes: usize,
}

impl Default for Glyphs {
    fn default() -> Self {
        Glyphs { context: ScaleContext::new(), faces: HashMap::default(), strikes: HashMap::default(), bytes: 0 }
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

/// Pixels per point on `canvas`. Points map to pixels as
/// `((x, y - origin_y) × scale)`.
fn scale_of(_canvas: &Canvas) -> f32 {
    // `canvas.scale` once the canvas carries it.
    1.0
}

/// Draw `run` into `canvas`, inside `clip` (layer coordinates).
pub(crate) fn draw_glyphs(canvas: &mut Canvas, cache: &mut Glyphs, run: &GlyphRun, clip: &Rect) {
    let scale = scale_of(canvas);
    let Some((cx0, cy0, cx1, cy1)) = pixel_clip(canvas, clip, scale) else { return };
    let size = run.size * scale;
    if !(size > 0.0 && size < 4096.0) {
        return;
    }
    let Glyphs { context, faces, strikes, bytes } = cache;
    let face = faces.entry(run.font).or_insert_with(|| fonts::face_data(run.font));
    let Some(face) = face.as_ref() else { return };
    let strike = strikes.entry((run.font, size.to_bits())).or_default();
    let (rgb, alpha) = channels(run.color);
    let (ox, oy) = (run.x * scale, (run.y - canvas.origin_y) * scale);
    // Glyphs rarely reach further than this from their origin.
    let reach = size * 2.0;
    let mut grown = 0;
    for g in run.glyphs.iter() {
        let x = ox + g.x * scale;
        let y = (oy + g.y * scale).round();
        if x + reach < cx0 as f32 || x - reach > cx1 as f32 || y + reach < cy0 as f32 || y - reach > cy1 as f32 {
            continue;
        }
        let mut px = x.floor();
        let mut bin = ((x - px) * SUBPIXEL).round() as u32;
        if bin as f32 >= SUBPIXEL {
            px += 1.0;
            bin = 0;
        }
        let key = g.id.wrapping_mul(SUBPIXEL as u32) + bin;
        let image = strike.glyphs.entry(key).or_insert_with(|| {
            let image = render(context, face, size, g.id, bin as f32 / SUBPIXEL);
            grown += image.as_ref().map_or(16, |i| i.width as usize * i.height as usize * 4 + 48);
            image
        });
        if let Some(image) = image {
            blit(canvas, image, px as i32 + image.left, y as i32 - image.top, (cx0, cy0, cx1, cy1), rgb, alpha);
        }
    }
    *bytes += grown;
    if *bytes > BUDGET {
        strikes.clear();
        *bytes = 0;
    }
}

fn render(context: &mut ScaleContext, face: &FaceData, size: f32, id: u32, offset: f32) -> Option<Image> {
    let font = swash::FontRef::from_index(face.font.data.data(), face.font.index as usize)?;
    let mut scaler = context.builder(font).size(size).hint(false).normalized_coords(face.coords.iter()).build();
    let sources = [Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline];
    let mut render = Render::new(&sources);
    render.format(Format::Alpha).offset(Vector::new(offset, 0.0));
    if face.embolden {
        render.embolden((size / 24.0).clamp(0.25, 2.0));
    }
    if face.skew != 0.0 {
        render.transform(Some(Transform::skew(Angle::from_degrees(face.skew), Angle::ZERO)));
    }
    let rendered = render.render(&mut scaler, u16::try_from(id).ok()?)?;
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
    use super::*;
    use crate::text::fonts::{Design, Family, FontSpec, resolve};

    fn face(family: &str) -> Option<FaceData> {
        let spec = FontSpec { family: Family::Named(family.into()), ..FontSpec::system(Design::Default, 0.0) };
        let face = resolve(&spec);
        let font = face.font.as_ref().filter(|_| &*face.family == family)?;
        fonts::face_data(fonts::register(font, &[], &Default::default()))
    }

    fn glyph(face: &FaceData, c: char) -> u32 {
        let font = skrifa::FontRef::from_index(face.font.data.data(), face.font.index).unwrap();
        skrifa::MetadataProvider::charmap(&font).map(c).map_or(0, |g| g.to_u32())
    }

    #[test]
    fn outlines_are_masks_and_emoji_are_color() {
        let mut context = ScaleContext::new();
        let sans = face("DejaVu Sans").expect("DejaVu Sans is installed");
        let a = render(&mut context, &sans, 20.0, glyph(&sans, 'a'), 0.0).expect("an image");
        assert!(matches!(a.pixels, Pixels::Mask(_)) && a.width > 4 && a.height > 4);
        let shifted = render(&mut context, &sans, 20.0, glyph(&sans, 'a'), 0.5).unwrap();
        let (Pixels::Mask(m0), Pixels::Mask(m1)) = (&a.pixels, &shifted.pixels) else { unreachable!() };
        assert_ne!(m0, m1, "a subpixel offset moves the coverage");
        assert!(render(&mut context, &sans, 20.0, glyph(&sans, ' '), 0.0).is_none(), "a space draws nothing");
        if let Some(emoji) = face("Noto Color Emoji") {
            let smile = render(&mut context, &emoji, 32.0, glyph(&emoji, '😀'), 0.0).expect("an emoji");
            let Pixels::Color(px) = &smile.pixels else { panic!("emoji come in color") };
            assert!(px.iter().any(|p| (p >> 16) & 0xff != p & 0xff), "not gray");
            assert!((28..=40).contains(&smile.width), "scaled to the size asked ({})", smile.width);
        }
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
    fn bench_glyph_rasterization() {
        let mut context = ScaleContext::new();
        let sans = face("DejaVu Sans").expect("DejaVu Sans is installed");
        let ids: Vec<u32> = (' '..='~').map(|c| glyph(&sans, c)).collect();
        for size in [13.0, 26.0] {
            let start = std::time::Instant::now();
            let mut count = 0;
            for round in 0..20 {
                for &id in &ids {
                    std::hint::black_box(render(&mut context, &sans, size, id, (round % 4) as f32 / SUBPIXEL));
                    count += 1;
                }
            }
            let per = start.elapsed().as_secs_f64() * 1e6 / f64::from(count);
            println!("rasterize a glyph at {size} px: {per:.2} µs ({:.0} glyphs/ms)", 1e3 / per);
        }
    }
}
