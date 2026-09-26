//! Writes the image files `conformance/tests/images.rs` reads, into the
//! directory given (`scripts/make-image-fixtures` passes
//! `conformance/tests/fixtures`). Every file is made here, from pixels
//! chosen here, with the image and png crates: no Apple tool touches them.

use std::io::Cursor;
use std::path::Path;

use image::{ImageFormat, Rgba, RgbaImage};

const RED: Rgba<u8> = Rgba([255, 0, 0, 255]);
const GREEN: Rgba<u8> = Rgba([0, 255, 0, 255]);
const BLUE: Rgba<u8> = Rgba([0, 0, 255, 255]);
const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);

fn encode(image: &RgbaImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    match format {
        ImageFormat::Jpeg => {
            let rgb = image::DynamicImage::ImageRgba8(image.clone()).into_rgb8();
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95);
            rgb.write_with_encoder(encoder).expect("a JPEG");
        }
        _ => image.write_to(&mut out, format).expect("encoded"),
    }
    out.into_inner()
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

/// A PNG with a `pHYs` chunk saying `dpi` dots per inch.
fn png_at(image: &RgbaImage, dpi: f64) -> Vec<u8> {
    let mut bytes = encode(image, ImageFormat::Png);
    let per_meter = (dpi / 0.0254).round() as u32;
    let mut body = b"pHYs".to_vec();
    body.extend_from_slice(&per_meter.to_be_bytes());
    body.extend_from_slice(&per_meter.to_be_bytes());
    body.push(1);
    let mut chunk = 9u32.to_be_bytes().to_vec();
    chunk.extend_from_slice(&body);
    chunk.extend_from_slice(&crc32(&body).to_be_bytes());
    // After the signature (8 bytes) and IHDR (25).
    bytes.splice(33..33, chunk);
    bytes
}

/// A JPEG with an EXIF segment giving `orientation`.
fn jpeg_oriented(image: &RgbaImage, orientation: u16) -> Vec<u8> {
    let mut bytes = encode(image, ImageFormat::Jpeg);
    // A big-endian TIFF header and one IFD with the Orientation tag.
    let mut exif = b"Exif\0\0MM\0\x2a\0\0\0\x08".to_vec();
    exif.extend_from_slice(&1u16.to_be_bytes());
    exif.extend_from_slice(&0x0112u16.to_be_bytes());
    exif.extend_from_slice(&3u16.to_be_bytes());
    exif.extend_from_slice(&1u32.to_be_bytes());
    exif.extend_from_slice(&orientation.to_be_bytes());
    exif.extend_from_slice(&[0, 0]);
    exif.extend_from_slice(&0u32.to_be_bytes());
    let mut segment = vec![0xff, 0xe1];
    segment.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    segment.extend_from_slice(&exif);
    // Right after the start-of-image marker.
    bytes.splice(2..2, segment);
    bytes
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: make-image-fixtures DIR");
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).expect("the fixtures directory");
    let write = |name: &str, bytes: Vec<u8>| std::fs::write(dir.join(name), bytes).expect("written");

    // 4 × 2: red, green, blue, white along the top; a half-transparent red
    // and three opaque grays below.
    let mut four = RgbaImage::new(4, 2);
    for (x, c) in [RED, GREEN, BLUE, WHITE].into_iter().enumerate() {
        four.put_pixel(x as u32, 0, c);
    }
    four.put_pixel(0, 1, Rgba([255, 0, 0, 128]));
    for x in 1..4 {
        four.put_pixel(x, 1, Rgba([64 * x as u8, 64 * x as u8, 64 * x as u8, 255]));
    }
    write("rgba-72dpi.png", png_at(&four, 72.0));
    write("rgba-144dpi.png", png_at(&four, 144.0));

    // Opaque, top half red and bottom half blue: which way up it's drawn.
    let halves = RgbaImage::from_fn(4, 4, |_, y| if y < 2 { RED } else { BLUE });
    write("halves.png", encode(&halves, ImageFormat::Png));

    // A 4 × 2 JPEG stored sideways: orientation 6 turns it upright as 2 × 4.
    let wide = RgbaImage::from_fn(4, 2, |x, _| if x < 2 { RED } else { BLUE });
    write("orientation-6.jpg", jpeg_oriented(&wide, 6));

    // GIF (first frame) and lossless WebP, 3 × 2.
    let small = RgbaImage::from_fn(3, 2, |x, y| [RED, GREEN, BLUE][((x + y) % 3) as usize]);
    write("small.gif", encode(&small, ImageFormat::Gif));
    write("small.webp", encode(&small, ImageFormat::WebP));

    write("garbage.png", b"\x89PNG\r\n\x1a\nthis is not really a PNG file at all".to_vec());
    write("huge.tiff", huge_tiff());
}

/// A gray TIFF whose header claims 2³¹ × 2³¹ pixels, over 16 bytes of
/// data: more bytes than memory holds, so no bitmap should come of it.
fn huge_tiff() -> Vec<u8> {
    const HUGE: u32 = 1 << 31;
    // (tag, type: 3 short or 4 long, value)
    let entries: [(u16, u16, u32); 9] = [
        (256, 4, HUGE), // width
        (257, 4, HUGE), // height
        (258, 3, 8),    // bits per sample
        (259, 3, 1),    // no compression
        (262, 3, 1),    // black is zero
        (273, 4, 0),    // strip offset, filled in below
        (277, 3, 1),    // samples per pixel
        (278, 4, HUGE), // rows per strip
        (279, 4, 16),   // strip byte count
    ];
    let data_at = 8 + 2 + entries.len() as u32 * 12 + 4;
    let mut out = b"II\x2a\0".to_vec();
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, kind, value) in entries {
        let value = if tag == 273 { data_at } else { value };
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        if kind == 3 {
            out.extend_from_slice(&(value as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
        } else {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&[0x80; 16]);
    out
}
