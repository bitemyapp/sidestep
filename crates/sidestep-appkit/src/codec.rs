//! Image files: reading their headers, decoding and encoding them, with
//! the pure-Rust codecs of the `image` crate (PNG, JPEG, GIF, WebP, BMP,
//! TIFF and ICO; nothing is linked from the system).
//!
//! Reading a file's header gives what `initWithData:` reports before any
//! pixel is decoded: its size, whether it has alpha, its EXIF orientation
//! (JPEG, TIFF and WebP carry one) and its density, from a PNG's `pHYs`
//! chunk or a JPEG's JFIF header, which sets its size in points (72 dots
//! per inch is a point a pixel). HEIC, AVIF and PDF aren't read.
//!
//! Decoded pixels are 8-bit RGBA, turned upright by the orientation unless
//! asked not to, straight or premultiplied as the caller needs. A file
//! whose pixels would take more than the codecs decode ([`MAX_BYTES`]) is
//! read as no image, header and all, rather than as one that never
//! decodes.

use std::io::Cursor;

use image::{ImageDecoder, ImageFormat, ImageReader, metadata::Orientation};
use objc2_app_kit::NSBitmapImageFileType;

/// The most bytes of decoded pixels a file may have: the `image` crate's
/// own limit on what it allocates to decode.
pub(crate) const MAX_BYTES: u64 = 512 << 20;

/// How a file is turned upright.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Turn(pub Orientation);

impl Turn {
    /// A quarter turn: width and height trade places.
    pub fn swaps(self) -> bool {
        matches!(
            self.0,
            Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH
        )
    }

    /// Whether turning it upright changes anything.
    pub fn turns(self) -> bool {
        self.0 != Orientation::NoTransforms
    }
}

/// What a file's header says.
#[derive(Clone, Debug)]
pub(crate) struct Header {
    /// Pixels, as stored (before the orientation turns them).
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub orientation: Turn,
    /// Dots per inch, across and down.
    pub dpi: (f64, f64),
}

fn reader(bytes: &[u8]) -> Option<ImageReader<Cursor<&[u8]>>> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    matches!(
        reader.format()?,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Gif
            | ImageFormat::WebP
            | ImageFormat::Bmp
            | ImageFormat::Tiff
            | ImageFormat::Ico
    )
    .then_some(reader)
}

/// The header of an image file, or `None` if it isn't one the codecs read.
pub(crate) fn header(bytes: &[u8]) -> Option<Header> {
    let reader = reader(bytes)?;
    let format = reader.format()?;
    let mut decoder = reader.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    let decoded = (u64::from(width) * u64::from(height)).checked_mul(4);
    if width == 0 || height == 0 || decoded.is_none_or(|b| b > MAX_BYTES) {
        return None;
    }
    // The GIF decoder gives every file an alpha channel; AppKit reports
    // one only for a first frame with a transparent color.
    let alpha = match format {
        ImageFormat::Gif => gif_transparent(bytes).unwrap_or(true),
        _ => decoder.color_type().has_alpha(),
    };
    let orientation = Turn(decoder.orientation().unwrap_or(Orientation::NoTransforms));
    let dpi = match format {
        ImageFormat::Png => png_density(bytes),
        ImageFormat::Jpeg => jfif_density(bytes),
        _ => None,
    }
    .unwrap_or((72.0, 72.0));
    Some(Header { width, height, alpha, orientation, dpi })
}

/// A PNG's `pHYs` density, in dots per inch, when given in meters.
fn png_density(bytes: &[u8]) -> Option<(f64, f64)> {
    let mut at = 8;
    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let kind = &bytes[at + 4..at + 8];
        if kind == b"IDAT" {
            return None;
        }
        if kind == b"pHYs" && len == 9 && at + 17 <= bytes.len() {
            let body = &bytes[at + 8..at + 17];
            let x = u32::from_be_bytes(body[0..4].try_into().ok()?);
            let y = u32::from_be_bytes(body[4..8].try_into().ok()?);
            if body[8] != 1 || x == 0 || y == 0 {
                return None;
            }
            // Pixels per meter to pixels per inch, to the nearest whole one
            // as AppKit reads them: 72 dpi is stored as 2835 per meter,
            // 72.009 per inch.
            return Some(((f64::from(x) * 0.0254).round(), (f64::from(y) * 0.0254).round()));
        }
        at += 12 + len;
    }
    None
}

/// Whether a GIF's first frame has a transparent color (its graphic
/// control extension says so).
fn gif_transparent(bytes: &[u8]) -> Option<bool> {
    // The header, then the screen descriptor and its color table.
    let packed = *bytes.get(10)?;
    let mut at = 13 + if packed & 0x80 != 0 { 3 << ((packed & 7) + 1) } else { 0 };
    loop {
        match *bytes.get(at)? {
            // An extension: its label, then sub-blocks up to an empty one.
            0x21 => {
                if *bytes.get(at + 1)? == 0xf9 {
                    return Some(bytes.get(at + 3)? & 1 != 0);
                }
                at += 2;
                loop {
                    let len = usize::from(*bytes.get(at)?);
                    at += 1 + len;
                    if len == 0 {
                        break;
                    }
                }
            }
            // The first image, with no control extension before it.
            _ => return Some(false),
        }
    }
}

/// A JPEG's JFIF density, in dots per inch.
fn jfif_density(bytes: &[u8]) -> Option<(f64, f64)> {
    let mut at = 2;
    while at + 4 <= bytes.len() && bytes[at] == 0xff {
        let marker = bytes[at + 1];
        let len = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if marker == 0xe0 && at + 4 + 12 <= bytes.len() && &bytes[at + 4..at + 9] == b"JFIF\0" {
            let body = &bytes[at + 4..];
            let units = body[7];
            let x = f64::from(u16::from_be_bytes([body[8], body[9]]));
            let y = f64::from(u16::from_be_bytes([body[10], body[11]]));
            return match units {
                1 if x > 0.0 && y > 0.0 => Some((x, y)),
                2 if x > 0.0 && y > 0.0 => Some((x * 2.54, y * 2.54)),
                _ => None,
            };
        }
        if marker == 0xda {
            return None;
        }
        at += 2 + len;
    }
    None
}

/// Decoded pixels: 8-bit RGBA rows, `width × 4` bytes apart.
pub(crate) struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The file's pixels, straight alpha, upright (or as stored).
pub(crate) fn decode_straight(bytes: &[u8], upright: bool) -> Option<Decoded> {
    let reader = reader(bytes)?;
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
    if upright {
        image.apply_orientation(orientation);
    }
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(Decoded { width, height, rgba: rgba.into_raw() })
}

/// The file's pixels, premultiplied, as canvases hold them.
pub(crate) fn decode(bytes: &[u8], upright: bool) -> Option<Decoded> {
    let mut d = decode_straight(bytes, upright)?;
    for p in d.rgba.as_chunks_mut::<4>().0 {
        let a = u32::from(p[3]);
        if a != 255 {
            for c in &mut p[..3] {
                *c = ((u32::from(*c) * a + 127) / 255) as u8;
            }
        }
    }
    Some(d)
}

/// Encode straight RGBA as `kind`; `quality` (0 to 1) for JPEG.
pub(crate) fn encode(
    kind: NSBitmapImageFileType,
    width: u32,
    height: u32,
    rgba: &[u8],
    quality: Option<f64>,
) -> Option<Vec<u8>> {
    let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let mut out = Cursor::new(Vec::new());
    match kind {
        NSBitmapImageFileType::PNG => image.write_to(&mut out, ImageFormat::Png).ok()?,
        NSBitmapImageFileType::TIFF => image.write_to(&mut out, ImageFormat::Tiff).ok()?,
        NSBitmapImageFileType::BMP => image.write_to(&mut out, ImageFormat::Bmp).ok()?,
        NSBitmapImageFileType::GIF => image.write_to(&mut out, ImageFormat::Gif).ok()?,
        NSBitmapImageFileType::JPEG => {
            let q = (quality.unwrap_or(0.9).clamp(0.0, 1.0) * 100.0).round().max(1.0) as u8;
            let rgb = image::DynamicImage::ImageRgba8(image).into_rgb8();
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
            rgb.write_with_encoder(encoder).ok()?;
        }
        _ => return None,
    }
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, dpm: Option<u32>) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(w, h, |x, _| image::Rgba([x as u8, 0, 0, 255]));
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        let mut bytes = out.into_inner();
        if let Some(d) = dpm {
            // A pHYs chunk right after IHDR (8 + 25 bytes in).
            let mut chunk = Vec::new();
            chunk.extend_from_slice(&9u32.to_be_bytes());
            let mut body = b"pHYs".to_vec();
            body.extend_from_slice(&d.to_be_bytes());
            body.extend_from_slice(&d.to_be_bytes());
            body.push(1);
            chunk.extend_from_slice(&body);
            chunk.extend_from_slice(&crc32(&body).to_be_bytes());
            bytes.splice(33..33, chunk);
        }
        bytes
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

    #[test]
    fn headers_give_size_alpha_and_density() {
        let h = header(&png(7, 3, None)).expect("a PNG");
        assert_eq!((h.width, h.height, h.alpha, h.dpi), (7, 3, true, (72.0, 72.0)));
        let h = header(&png(7, 3, Some(5669))).expect("a PNG at 144 dpi");
        assert_eq!(h.dpi, (144.0, 144.0));
        assert!(header(b"not an image at all").is_none());
        assert!(header(&[]).is_none());
        // A header claiming more pixels than can be decoded is no image.
        assert!(header(include_bytes!("../../../conformance/tests/fixtures/huge.tiff")).is_none());
    }

    #[test]
    fn gifs_have_alpha_only_with_a_transparent_color() {
        let gif = |transparent: Option<u8>| {
            let mut frame = image::Frame::new(image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])));
            if transparent.is_some() {
                frame =
                    image::Frame::new(image::RgbaImage::from_fn(2, 2, |x, _| image::Rgba([255, 0, 0, 255 * x as u8])));
            }
            let mut out = Vec::new();
            image::codecs::gif::GifEncoder::new(&mut out).encode_frames([frame]).unwrap();
            out
        };
        assert!(!header(&gif(None)).unwrap().alpha);
        assert!(header(&gif(Some(0))).unwrap().alpha);
    }

    #[test]
    fn round_trips_through_every_encoder_that_keeps_pixels() {
        let rgba: Vec<u8> = (0..4 * 3 * 4).map(|i| if i % 4 == 3 { 255 } else { (i * 7) as u8 }).collect();
        for kind in [NSBitmapImageFileType::PNG, NSBitmapImageFileType::TIFF, NSBitmapImageFileType::BMP] {
            let bytes = encode(kind, 4, 3, &rgba, None).expect("encoded");
            let back = decode_straight(&bytes, true).expect("decoded");
            assert_eq!((back.width, back.height), (4, 3));
            assert_eq!(back.rgba, rgba, "{kind:?}");
        }
        let jpeg = encode(NSBitmapImageFileType::JPEG, 4, 3, &rgba, Some(0.5)).expect("a JPEG");
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        assert!(encode(NSBitmapImageFileType::JPEG2000, 4, 3, &rgba, None).is_none());
    }

    #[test]
    fn premultiplying_scales_color_by_alpha() {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([200, 100, 50, 128]));
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        let d = decode(&out.into_inner(), true).unwrap();
        assert_eq!(d.rgba, [100, 50, 25, 128]);
    }
}
