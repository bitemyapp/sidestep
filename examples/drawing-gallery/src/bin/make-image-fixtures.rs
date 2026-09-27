//! Writes the image files `conformance/tests/images.rs` reads, into the
//! directory given (`scripts/make-image-fixtures` passes
//! `conformance/tests/fixtures`). Every file is made here, from pixels
//! chosen here, with the image and png crates: no Apple tool touches them.

use std::io::Cursor;
use std::path::Path;

use image::codecs::gif::Repeat;
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

    // An animated GIF, 4 × 4: a red, a green and a blue frame, shown for
    // 0.1, 0.2 and 0.3 seconds, looping forever; the same played once (no
    // loop count) and twice (played again once).
    write("frames.gif", frames_gif(Some(Repeat::Infinite)));
    write("once.gif", frames_gif(None));
    write("twice.gif", frames_gif(Some(Repeat::Finite(1))));

    write("garbage.png", b"\x89PNG\r\n\x1a\nthis is not really a PNG file at all".to_vec());
    write("huge.tiff", huge_tiff());

    // For ImageIO (conformance/tests/imageio.rs). In thirds, red, green and
    // blue across: a 300 × 200 JPEG stored sideways (orientation 6) with an
    // EXIF block of camera tags and a 30 × 20 green thumbnail (ImageIO
    // passes over thumbnails of some files, of small ones among them; this
    // one it uses), and a 48 × 32 plain JPEG.
    let thirds = |w: u32, h: u32| RgbaImage::from_fn(w, h, |x, _| [RED, GREEN, BLUE][(x * 3 / w) as usize]);
    let thumb = encode(&RgbaImage::from_pixel(30, 20, GREEN), ImageFormat::Jpeg);
    write("exif-thumb.jpg", with_exif(&encode(&thirds(300, 200), ImageFormat::Jpeg), &camera_exif(&thumb)));
    write("plain.jpg", encode(&thirds(48, 32), ImageFormat::Jpeg));
    // 48 × 29, for thumbnails' rounding.
    write("wide.png", encode(&thirds(48, 29), ImageFormat::Png));
    // Gray, and 16-bit RGB, 4 × 2.
    let gray = image::GrayImage::from_fn(4, 2, |x, _| image::Luma([x as u8 * 60]));
    write("gray.png", encode_dynamic(image::DynamicImage::ImageLuma8(gray), ImageFormat::Png));
    let rgb16 =
        image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_fn(4, 2, |x, _| image::Rgb([x as u16 * 16000, 0, 0]));
    write("rgb16.png", encode_dynamic(image::DynamicImage::ImageRgb16(rgb16), ImageFormat::Png));
    // A PNG with gamma, sRGB and text chunks, 4 × 2 red.
    write("text.png", png_with_chunks(&RgbaImage::from_pixel(4, 2, RED)));
    // An animated WebP, 4 × 4: red for 0.1 seconds, blue for 0.25, looping
    // forever.
    let red = RgbaImage::from_pixel(4, 4, RED);
    let blue = RgbaImage::from_pixel(4, 4, BLUE);
    write("frames.webp", animated_webp(&[(red, 100), (blue, 250)], 0));
}

fn encode_dynamic(image: image::DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, format).expect("encoded");
    out.into_inner()
}

/// `jpeg` with an APP1 EXIF segment holding the TIFF structure `tiff`,
/// right after the start-of-image marker.
fn with_exif(jpeg: &[u8], tiff: &[u8]) -> Vec<u8> {
    let mut segment = vec![0xff, 0xe1];
    segment.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
    segment.extend_from_slice(b"Exif\0\0");
    segment.extend_from_slice(tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&segment);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// A TIFF value: SHORT, LONG, RATIONAL, ASCII or UNDEFINED.
enum Tag {
    Short(u16),
    Long(u32),
    Rational(u32, u32),
    Ascii(&'static str),
    Undefined(&'static [u8]),
}

/// A big-endian EXIF block (a TIFF structure, written from the EXIF 2.3
/// specification): IFD0 with the image's description, maker, orientation 6
/// and density, its EXIF directory with exposure tags, and IFD1 holding a
/// JPEG `thumb`.
fn camera_exif(thumb: &[u8]) -> Vec<u8> {
    let ifd0 = vec![
        (0x010e, Tag::Ascii("A test")),
        (0x010f, Tag::Ascii("Sidestep")),
        (0x0110, Tag::Ascii("Fixture")),
        (0x0112, Tag::Short(6)),
        (0x011a, Tag::Rational(300, 1)),
        (0x011b, Tag::Rational(300, 1)),
        (0x0128, Tag::Short(2)),
        (0x0131, Tag::Ascii("make-image-fixtures")),
    ];
    let exif = vec![
        (0x829a, Tag::Rational(1, 100)),
        (0x829d, Tag::Rational(28, 10)),
        (0x8827, Tag::Short(200)),
        (0x9000, Tag::Undefined(b"0232")),
        (0x9003, Tag::Ascii("2026:09:27 10:11:12")),
        (0x920a, Tag::Rational(50, 1)),
        (0xa001, Tag::Short(1)),
        (0xa002, Tag::Long(300)),
        (0xa003, Tag::Long(200)),
    ];
    // IFD0 (with the EXIF pointer), then the EXIF directory, then IFD1
    // (compression 6, the thumbnail's offset and length), each followed by
    // its values longer than four bytes, then the thumbnail.
    let size = |t: &Tag| match t {
        Tag::Short(_) => 2,
        Tag::Long(_) => 4,
        Tag::Rational(..) => 8,
        Tag::Ascii(s) => s.len() + 1,
        Tag::Undefined(b) => b.len(),
    };
    let extra = |tags: &[(u16, Tag)]| tags.iter().map(|(_, t)| if size(t) > 4 { size(t) } else { 0 }).sum::<usize>();
    let ifd0_at = 8;
    let exif_at = ifd0_at + 2 + 12 * (ifd0.len() + 1) + 4 + extra(&ifd0);
    let ifd1_at = exif_at + 2 + 12 * exif.len() + 4 + extra(&exif);
    let thumb_at = ifd1_at + 2 + 12 * 3 + 4;
    let mut out = b"MM\0\x2a".to_vec();
    out.extend_from_slice(&(ifd0_at as u32).to_be_bytes());
    let directory = |out: &mut Vec<u8>, at: usize, tags: &[(u16, Tag)], pointer: Option<u32>, next: usize| {
        let count = tags.len() + usize::from(pointer.is_some());
        let mut data_at = at + 2 + 12 * count + 4;
        let mut data = Vec::new();
        out.extend_from_slice(&(count as u16).to_be_bytes());
        let mut entries: Vec<(u16, u16, u32, Vec<u8>)> = tags
            .iter()
            .map(|(tag, t)| match t {
                Tag::Short(v) => (*tag, 3, 1, v.to_be_bytes().to_vec()),
                Tag::Long(v) => (*tag, 4, 1, v.to_be_bytes().to_vec()),
                Tag::Rational(a, b) => (*tag, 5, 1, [a.to_be_bytes(), b.to_be_bytes()].concat()),
                Tag::Ascii(s) => (*tag, 2, s.len() as u32 + 1, [s.as_bytes(), &[0]].concat()),
                Tag::Undefined(b) => (*tag, 7, b.len() as u32, b.to_vec()),
            })
            .collect();
        if let Some(p) = pointer {
            entries.push((0x8769, 4, 1, p.to_be_bytes().to_vec()));
            entries.sort_by_key(|e| e.0);
        }
        for (tag, kind, n, mut bytes) in entries {
            out.extend_from_slice(&tag.to_be_bytes());
            out.extend_from_slice(&kind.to_be_bytes());
            out.extend_from_slice(&n.to_be_bytes());
            if bytes.len() <= 4 {
                bytes.resize(4, 0);
                out.extend_from_slice(&bytes);
            } else {
                out.extend_from_slice(&(data_at as u32).to_be_bytes());
                data_at += bytes.len();
                data.extend_from_slice(&bytes);
            }
        }
        out.extend_from_slice(&(next as u32).to_be_bytes());
        out.extend_from_slice(&data);
    };
    directory(&mut out, ifd0_at, &ifd0, Some(exif_at as u32), ifd1_at);
    directory(&mut out, exif_at, &exif, None, 0);
    let ifd1 = [(0x0103, Tag::Short(6)), (0x0201, Tag::Long(thumb_at as u32)), (0x0202, Tag::Long(thumb.len() as u32))];
    directory(&mut out, ifd1_at, &ifd1, None, 0);
    assert_eq!(out.len(), thumb_at);
    out.extend_from_slice(thumb);
    out
}

/// A PNG of `image` with `gAMA`, `sRGB` and text chunks (a title, an author
/// and an international description) after its header.
fn png_with_chunks(image: &RgbaImage) -> Vec<u8> {
    let mut bytes = encode(image, ImageFormat::Png);
    let mut chunks = Vec::new();
    for (kind, body) in [
        (b"gAMA", &45455u32.to_be_bytes()[..]),
        (b"sRGB", &[0][..]),
        (b"tEXt", &b"Title\0A title"[..]),
        (b"tEXt", &b"Author\0Someone"[..]),
        (b"iTXt", &b"Description\0\0\0en\0\0Caf\xc3\xa9"[..]),
    ] {
        let mut named = kind.to_vec();
        named.extend_from_slice(body);
        chunks.extend_from_slice(&(body.len() as u32).to_be_bytes());
        chunks.extend_from_slice(&named);
        chunks.extend_from_slice(&crc32(&named).to_be_bytes());
    }
    bytes.splice(33..33, chunks);
    bytes
}

/// A RIFF chunk.
fn riff_chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn u24(n: u32) -> [u8; 3] {
    let b = n.to_le_bytes();
    [b[0], b[1], b[2]]
}

/// An animated WebP (written from the WebP container specification) of
/// whole frames, each a lossless still's image data shown for its
/// milliseconds, played `loops` times (0 for ever).
fn animated_webp(frames: &[(RgbaImage, u32)], loops: u16) -> Vec<u8> {
    let (w, h) = frames[0].0.dimensions();
    let mut body = b"WEBP".to_vec();
    // Animation and alpha flags, then the canvas less one.
    let mut vp8x = vec![0x02 | 0x10, 0, 0, 0];
    vp8x.extend_from_slice(&u24(w - 1));
    vp8x.extend_from_slice(&u24(h - 1));
    body.extend(riff_chunk(b"VP8X", &vp8x));
    let mut anim = vec![0, 0, 0, 0];
    anim.extend_from_slice(&loops.to_le_bytes());
    body.extend(riff_chunk(b"ANIM", &anim));
    for (image, ms) in frames {
        let still = encode(image, ImageFormat::WebP);
        // The still file's image data chunks, after its RIFF header.
        let mut at = 12;
        let mut data = Vec::new();
        while at + 8 <= still.len() {
            let id = &still[at..at + 4];
            let len = u32::from_le_bytes(still[at + 4..at + 8].try_into().expect("a length")) as usize;
            if id == b"VP8L" || id == b"VP8 " || id == b"ALPH" {
                data.extend_from_slice(&still[at..at + 8 + len + (len & 1)]);
            }
            at += 8 + len + (len & 1);
        }
        let mut anmf = Vec::new();
        anmf.extend_from_slice(&u24(0));
        anmf.extend_from_slice(&u24(0));
        anmf.extend_from_slice(&u24(w - 1));
        anmf.extend_from_slice(&u24(h - 1));
        anmf.extend_from_slice(&u24(*ms));
        anmf.push(0);
        anmf.extend_from_slice(&data);
        body.extend(riff_chunk(b"ANMF", &anmf));
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Three whole frames, red, green and blue, shown for 10, 20 and 30
/// hundredths of a second, played again as `repeat` says (a NETSCAPE2.0
/// loop count), or once without one.
fn frames_gif(repeat: Option<Repeat>) -> Vec<u8> {
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame};
    let mut out = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut out);
        if let Some(repeat) = repeat {
            encoder.set_repeat(repeat).expect("a loop count");
        }
        let ms = |n: u32| Delay::from_numer_denom_ms(n, 1);
        let red = Frame::from_parts(RgbaImage::from_pixel(4, 4, RED), 0, 0, ms(100));
        let green = Frame::from_parts(RgbaImage::from_pixel(4, 4, GREEN), 0, 0, ms(200));
        let blue = Frame::from_parts(RgbaImage::from_pixel(4, 4, BLUE), 0, 0, ms(300));
        encoder.encode_frames([red, green, blue]).expect("frames");
    }
    out
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
