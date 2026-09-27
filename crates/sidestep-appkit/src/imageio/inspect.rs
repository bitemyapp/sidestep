//! What ImageIO reports of a file, read once when a source gets its data:
//! how many images it has and each one's properties, the file's own
//! properties, and where an embedded thumbnail is. Only headers and
//! metadata blocks are read; no pixel is decoded.
//!
//! The dictionaries follow what ImageIO gives on macOS (measured with files
//! made by the `image` crate): pixel size, depth, color model, alpha,
//! orientation, density and profile name at the top, and the format's
//! own dictionary beside them: `{PNG}` (interlacing, density, gamma,
//! chromaticities, rendering intent, text), `{JFIF}` (version, density,
//! progressive), `{GIF}` (each frame's delay, clamped and not; the file's
//! canvas, frames, color map and loop count), `{WebP}` (the same for the
//! file), `{TIFF}` and `{Exif}` from a TIFF file's first directory or a
//! JPEG's, PNG's or WebP's EXIF block. Keys the codecs give no value for
//! are left out, as ImageIO leaves out what a file doesn't have.

use std::ops::Range;

use super::exif::{self, Value};
use super::format::Kind;
use super::plist::{Dict, P};

/// A file, as ImageIO describes it.
#[derive(Debug)]
pub(crate) struct Info {
    pub kind: Kind,
    /// How many images the file has.
    pub count: usize,
    /// The images whose headers could be read, first to last: fewer than
    /// `count` when the file's header can't be (those images have no
    /// properties and make no image, as ImageIO's don't).
    pub images: Vec<Image>,
    /// The file's format dictionary (`{GIF}`, `{WebP}`), if it has one.
    pub file: Dict,
    /// A JPEG's EXIF thumbnail: where its bytes are in the file.
    pub thumbnail: Option<Range<usize>>,
    /// The file is an animated GIF or WebP (it has more than one frame).
    pub animated: bool,
}

/// One image of a file.
#[derive(Debug)]
pub(crate) struct Image {
    /// As stored, before the orientation turns it.
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    /// The EXIF orientation, 1 to 8.
    pub orientation: u16,
    pub props: Dict,
}

/// The name ImageIO gives the profile of files it takes as sRGB.
const SRGB: &str = "sRGB IEC61966-2.1";

/// Describe `bytes`, or `None` if they aren't a file of a type read here
/// (or an icon file ImageIO refuses: one whose first icon is smaller than
/// 12 pixels either way, as its directory gives them).
pub(crate) fn inspect(bytes: &[u8]) -> Option<Info> {
    let kind = Kind::sniff(bytes)?;
    if kind == Kind::Ico && ico_first(bytes).is_some_and(|(w, h, _)| w < 12 || h < 12) {
        return None;
    }
    let mut info = Info { kind, count: 0, images: Vec::new(), file: Dict::new(), thumbnail: None, animated: false };
    let Some(d) = crate::codec::describe(bytes) else {
        // A file of a known type the codecs can't read: one image with no
        // properties, as ImageIO counts it (a TIFF whose directory can't
        // be read has none).
        info.count = usize::from(kind != Kind::Tiff);
        return Some(info);
    };
    let h = &d.header;
    let mut props = Dict::new();
    let (model, depth) = color_model(d.color);
    props.set("ColorModel", P::Str(model.into()));
    props.set("Depth", P::Long(depth));
    if h.alpha {
        props.set("HasAlpha", P::Bool(true));
    }
    props.set("PixelHeight", P::Long(i64::from(h.height)));
    props.set("PixelWidth", P::Long(i64::from(h.width)));
    let mut orientation = 1;
    // The EXIF block's directories (a TIFF file's own): a JPEG's read in
    // place, so an embedded thumbnail can be found in the file.
    let exif_bytes: Option<&[u8]> = match kind {
        Kind::Tiff => Some(bytes),
        Kind::Jpeg => exif::jpeg_exif(bytes),
        _ => d.exif.as_deref().map(|b| b.strip_prefix(b"Exif\0\0").unwrap_or(b)),
    };
    let tiff = exif_bytes.and_then(exif::parse);
    let mut dpi = None;
    if let (Some(t), Some(block)) = (&tiff, exif_bytes) {
        let tiff_dict = map_tags(&t.ifd0, TIFF_TAGS);
        if let Some(o) = exif::get(&t.ifd0, 0x0112).and_then(Value::number)
            && (1.0..=8.0).contains(&o)
        {
            orientation = o as u16;
            props.set("Orientation", P::Int(o as i32));
        }
        dpi = tiff_dpi(&t.ifd0, kind);
        if !tiff_dict.is_empty() {
            props.set("{TIFF}", P::Dict(tiff_dict));
        }
        let exif_dict = map_tags(&t.exif, EXIF_TAGS);
        if !exif_dict.is_empty() {
            props.set("{Exif}", P::Dict(exif_dict));
        }
        if kind == Kind::Jpeg
            && let Some(thumb) = t.thumbnail(block)
        {
            // A slice of the file: where it starts in it.
            let start = thumb.as_ptr() as usize - bytes.as_ptr() as usize;
            info.thumbnail = Some(start..start + thumb.len());
        }
    }
    let mut profile = d.icc.as_deref().and_then(icc_description);
    match kind {
        Kind::Png => {
            let png = png_dict(bytes, &mut dpi);
            if png.get("sRGBIntent").is_some() && profile.is_none() {
                profile = Some(SRGB.into());
            }
            png_elsewhere(&png, &mut props);
            props.set("{PNG}", P::Dict(png));
        }
        // An icon stored as a PNG has the PNG's dictionary.
        Kind::Ico => {
            if let Some((_, _, icon)) = ico_first(bytes)
                && icon.starts_with(b"\x89PNG\r\n\x1a\n")
            {
                props.set("{PNG}", P::Dict(png_dict(icon, &mut None)));
            }
        }
        Kind::Bmp => dpi = bmp_dpi(bytes),
        Kind::Jpeg => {
            let (jfif, jfif_dpi) = jfif_dict(bytes);
            if let Some(j) = jfif {
                props.set("{JFIF}", P::Dict(j));
            }
            dpi = dpi.or(jfif_dpi);
            profile = profile.or(Some(SRGB.into()));
        }
        Kind::Gif => profile = profile.or(Some(SRGB.into())),
        _ => {}
    }
    if let Some((x, y)) = dpi {
        props.set("DPIHeight", P::Float(y as f32));
        props.set("DPIWidth", P::Float(x as f32));
    }
    if let Some(p) = profile {
        props.set("ProfileName", P::Str(p));
    }
    iptc(&mut props);
    let image = Image { width: h.width, height: h.height, alpha: h.alpha, orientation, props };
    match kind {
        Kind::Gif => gif(bytes, image, &mut info),
        Kind::WebP => webp(bytes, image, &mut info),
        _ => {
            info.count = 1;
            info.images.push(image);
        }
    }
    Some(info)
}

/// An icon file's first icon: its width and height as the directory gives
/// them (0 is 256) and its bytes.
fn ico_first(bytes: &[u8]) -> Option<(u32, u32, &[u8])> {
    let entry = bytes.get(6..22)?;
    let side = |b: u8| if b == 0 { 256 } else { u32::from(b) };
    let len = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
    let at = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
    let icon = bytes.get(at..at.saturating_add(len)).or_else(|| bytes.get(at..))?;
    Some((side(entry[0]), side(entry[1]), icon))
}

/// A BMP's density from its info header's pixels per meter, as ImageIO
/// gives it (measured): none unless both are at least 10 dots per inch
/// (394 per meter); within a twentieth of 72 or 96, those.
fn bmp_dpi(bytes: &[u8]) -> Option<(f64, f64)> {
    let header = u32::from_le_bytes(bytes.get(14..18)?.try_into().ok()?);
    if header < 40 {
        return None;
    }
    let ppm = |at: usize| Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let dpi = |ppm: i32| {
        let d = f64::from(ppm) * 0.0254;
        [72.0, 96.0].into_iter().find(|n| (d - n).abs() < 0.05).unwrap_or(d)
    };
    let (x, y) = (ppm(38)?, ppm(42)?);
    let enough = |ppm: i32| f64::from(ppm) * 0.0254 >= 10.0;
    (enough(x) && enough(y)).then(|| (dpi(x), dpi(y)))
}

/// What ImageIO lists of a PNG's text in other dictionaries too (measured):
/// a comment as the EXIF user comment, the copyright as the TIFF one.
fn png_elsewhere(png: &Dict, props: &mut Dict) {
    for (text, dict, key) in [("Comment", "{Exif}", "UserComment"), ("Copyright", "{TIFF}", "Copyright")] {
        let Some(value) = png.get(text) else { continue };
        let mut d = match props.get(dict) {
            Some(P::Dict(d)) => d.clone(),
            _ => Dict::new(),
        };
        if d.get(key).is_none() {
            d.set(key, value.clone());
            props.set(dict, P::Dict(d));
        }
    }
}

/// The `{IPTC}` ImageIO makes of other metadata (measured): the caption of
/// a PNG's description or a TIFF directory's image description, the object
/// name of a PNG's title.
fn iptc(props: &mut Dict) {
    let in_dict = |dict: &str, key: &str| match props.get(dict) {
        Some(P::Dict(d)) => d.get(key).cloned(),
        _ => None,
    };
    let caption = in_dict("{PNG}", "Description").or_else(|| in_dict("{TIFF}", "ImageDescription"));
    let name = in_dict("{PNG}", "Title");
    let mut d = Dict::new();
    if let Some(c) = caption {
        d.set("Caption/Abstract", c);
    }
    if let Some(n) = name {
        d.set("ObjectName", n);
    }
    if !d.is_empty() {
        props.set("{IPTC}", P::Dict(d));
    }
}

/// `ColorModel` and `Depth` of the samples as stored.
fn color_model(color: image::ExtendedColorType) -> (&'static str, i64) {
    use image::ExtendedColorType as C;
    let gray =
        matches!(color, C::L1 | C::L2 | C::L4 | C::L8 | C::L16 | C::La1 | C::La2 | C::La4 | C::La8 | C::La16 | C::A8);
    let cmyk = matches!(color, C::Cmyk8 | C::Cmyk16);
    let channels = i64::from(color.channel_count().max(1));
    let depth = (i64::from(color.bits_per_pixel()) / channels).max(1);
    (
        if gray {
            "Gray"
        } else if cmyk {
            "CMYK"
        } else {
            "RGB"
        },
        depth,
    )
}

/// A delay as ImageIO clamps it: shorter than about a hundredth of a
/// second shows for a tenth (as `crate::codec::gif_frames` counts it).
fn clamped(seconds: f64) -> f64 {
    if seconds < 0.011 { 0.1 } else { seconds }
}

fn delays(unclamped: f64) -> Dict {
    Dict(vec![("DelayTime", P::Double(clamped(unclamped))), ("UnclampedDelayTime", P::Double(unclamped))])
}

/// A GIF's frames: each an image of the canvas's size, with its delay and
/// its own transparency; the file's `{GIF}`.
fn gif(bytes: &[u8], first: Image, info: &mut Info) {
    let Some(g) = gif_blocks(bytes) else {
        info.count = 1;
        info.images.push(first);
        return;
    };
    let frames: Vec<(f64, bool)> = if g.frames.is_empty() { vec![(0.0, first.alpha)] } else { g.frames.clone() };
    for &(delay, transparent) in &frames {
        let mut props = first.props.clone();
        props.set("PixelHeight", P::Long(i64::from(g.height)));
        props.set("PixelWidth", P::Long(i64::from(g.width)));
        props.0.retain(|(k, _)| *k != "HasAlpha");
        if transparent {
            props.set("HasAlpha", P::Bool(true));
        }
        props.set("{GIF}", P::Dict(delays(delay)));
        info.images.push(Image { width: g.width, height: g.height, alpha: transparent, orientation: 1, props });
    }
    info.count = frames.len();
    info.animated = frames.len() > 1;
    let mut file = Dict::new();
    file.set("CanvasPixelHeight", P::Int(g.height as i32));
    file.set("CanvasPixelWidth", P::Int(g.width as i32));
    file.set("FrameInfo", P::Array(frames.iter().map(|&(d, _)| P::Dict(delays(d))).collect()));
    file.set("HasGlobalColorMap", P::Bool(g.global_map));
    file.set("LoopCount", P::Int(g.loops as i32));
    info.file.set("{GIF}", P::Dict(file));
}

/// What a GIF's blocks say, read without decoding.
#[derive(Clone, Debug)]
struct GifBlocks {
    width: u32,
    height: u32,
    global_map: bool,
    /// Each frame's delay (seconds, unclamped) and whether it has a
    /// transparent color.
    frames: Vec<(f64, bool)>,
    /// Times played: once without a `NETSCAPE2.0` block, for ever (0) when
    /// it says 0, and once more than it says otherwise.
    loops: u32,
}

fn gif_blocks(bytes: &[u8]) -> Option<GifBlocks> {
    if !bytes.starts_with(b"GIF8") {
        return None;
    }
    let width = u32::from(u16::from_le_bytes([*bytes.get(6)?, *bytes.get(7)?]));
    let height = u32::from(u16::from_le_bytes([*bytes.get(8)?, *bytes.get(9)?]));
    let packed = *bytes.get(10)?;
    let global_map = packed & 0x80 != 0;
    let mut at = 13 + if global_map { 3 << ((packed & 7) + 1) } else { 0 };
    let skip = |mut at: usize| -> Option<usize> {
        loop {
            let len = usize::from(*bytes.get(at)?);
            at += 1 + len;
            if len == 0 {
                return Some(at);
            }
        }
    };
    let mut g = GifBlocks { width, height, global_map, frames: Vec::new(), loops: 1 };
    let (mut delay, mut transparent) = (0u16, false);
    // A file cut short keeps the frames read before the cut.
    while let Some(&block) = bytes.get(at) {
        match block {
            0x21 => {
                match bytes.get(at + 1) {
                    Some(0xf9) => {
                        let (Some(&flags), Some(&lo), Some(&hi)) =
                            (bytes.get(at + 3), bytes.get(at + 4), bytes.get(at + 5))
                        else {
                            break;
                        };
                        transparent = flags & 1 != 0;
                        delay = u16::from_le_bytes([lo, hi]);
                    }
                    Some(0xff)
                        if bytes.get(at + 3..at + 14) == Some(b"NETSCAPE2.0".as_slice())
                            && bytes.get(at + 14..at + 16) == Some([3, 1].as_slice()) =>
                    {
                        let (Some(&lo), Some(&hi)) = (bytes.get(at + 16), bytes.get(at + 17)) else { break };
                        let again = u32::from(u16::from_le_bytes([lo, hi]));
                        g.loops = if again == 0 { 0 } else { again + 1 };
                    }
                    _ => {}
                }
                let Some(next) = skip(at + 2) else { break };
                at = next;
            }
            0x2c => {
                let Some(&local) = bytes.get(at + 9) else { break };
                let table = if local & 0x80 != 0 { 3 << ((local & 7) + 1) } else { 0 };
                let Some(next) = skip(at + 10 + table + 1) else { break };
                at = next;
                g.frames.push((f64::from(delay) / 100.0, transparent));
                (delay, transparent) = (0, false);
            }
            _ => break,
        }
    }
    Some(g)
}

/// A WebP's frames and the file's `{WebP}`: one frame of a still file
/// (shown for a tenth, unclamped nothing, played once, as ImageIO reports
/// it), or each frame of an animated one with its duration.
fn webp(bytes: &[u8], first: Image, info: &mut Info) {
    let chunks = riff_chunks(bytes);
    let canvas = chunks.iter().find(|(id, _)| id == b"VP8X").and_then(|(_, b)| {
        let w = u32::from_le_bytes([*b.get(4)?, *b.get(5)?, *b.get(6)?, 0]) + 1;
        let h = u32::from_le_bytes([*b.get(7)?, *b.get(8)?, *b.get(9)?, 0]) + 1;
        Some((w, h, b.first().is_some_and(|f| f & 0x02 != 0)))
    });
    let (width, height) = canvas.map_or((first.width, first.height), |(w, h, _)| (w, h));
    let animated = canvas.is_some_and(|(_, _, a)| a);
    let frames: Vec<f64> = if animated {
        chunks
            .iter()
            .filter(|(id, _)| id == b"ANMF")
            .filter_map(|(_, b)| {
                Some(f64::from(u32::from_le_bytes([*b.get(12)?, *b.get(13)?, *b.get(14)?, 0])) / 1000.0)
            })
            .collect()
    } else {
        Vec::new()
    };
    let loops = if animated {
        chunks
            .iter()
            .find(|(id, _)| id == b"ANIM")
            .and_then(|(_, b)| Some(u32::from(u16::from_le_bytes([*b.get(4)?, *b.get(5)?]))))
            .unwrap_or(0)
    } else {
        1
    };
    let mut file = Dict::new();
    file.set("CanvasPixelHeight", P::Int(height as i32));
    file.set("CanvasPixelWidth", P::Int(width as i32));
    if frames.is_empty() {
        file.set("FrameInfo", P::Array(vec![P::Dict(delays(0.0))]));
        info.count = 1;
        info.images.push(first);
    } else {
        file.set("FrameInfo", P::Array(frames.iter().map(|&d| P::Dict(delays(d))).collect()));
        for &delay in &frames {
            let mut props = first.props.clone();
            props.set("PixelHeight", P::Long(i64::from(height)));
            props.set("PixelWidth", P::Long(i64::from(width)));
            props.set("{WebP}", P::Dict(delays(delay)));
            info.images.push(Image { width, height, alpha: first.alpha, orientation: 1, props });
        }
        info.count = frames.len();
        info.animated = frames.len() > 1;
    }
    file.set("LoopCount", P::Int(loops as i32));
    info.file.set("{WebP}", P::Dict(file));
}

/// A RIFF file's top-level chunks: each id and body.
fn riff_chunks(bytes: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 12;
    while let Some(head) = bytes.get(at..at + 8) {
        let id: [u8; 4] = head[..4].try_into().unwrap_or_default();
        let len = u32::from_le_bytes(head[4..8].try_into().unwrap_or_default()) as usize;
        let Some(body) = bytes.get(at + 8..(at + 8).saturating_add(len)) else { break };
        out.push((id, body));
        at += 8 + len + (len & 1);
        if out.len() > 100_000 {
            break;
        }
    }
    out
}

/// A PNG's `{PNG}` from its chunks before the image data; `dpi` gets the
/// density a `pHYs` in meters gives.
fn png_dict(bytes: &[u8], dpi: &mut Option<(f64, f64)>) -> Dict {
    let mut d = Dict::new();
    let mut at = 8;
    let be32 = |b: &[u8], i: usize| b.get(i..i + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]));
    while let Some(len) = be32(bytes, at) {
        let len = len as usize;
        let (Some(kind), Some(body)) = (bytes.get(at + 4..at + 8), bytes.get(at + 8..(at + 8).saturating_add(len)))
        else {
            break;
        };
        match kind {
            b"IHDR" => {
                if let Some(&i) = body.get(12) {
                    d.set("InterlaceType", P::Int(i32::from(i)));
                }
            }
            b"pHYs" if body.len() == 9 => {
                let (x, y) = (be32(body, 0).unwrap_or(0), be32(body, 4).unwrap_or(0));
                if body[8] == 1 {
                    d.set("XPixelsPerMeter", P::Int(x as i32));
                    d.set("YPixelsPerMeter", P::Int(y as i32));
                    if x > 0 && y > 0 {
                        *dpi = Some(((f64::from(x) * 0.0254).round(), (f64::from(y) * 0.0254).round()));
                    }
                }
            }
            b"gAMA" if body.len() == 4 => d.set("Gamma", P::Double(f64::from(be32(body, 0).unwrap_or(0)) / 100_000.0)),
            b"sRGB" if !body.is_empty() => d.set("sRGBIntent", P::Int(i32::from(body[0]))),
            b"cHRM" if body.len() == 32 => {
                let values = (0..8).map(|i| P::Double(f64::from(be32(body, 4 * i).unwrap_or(0)) / 100_000.0));
                d.set("Chromaticities", P::Array(values.collect()));
            }
            b"tEXt" | b"iTXt" => {
                if let Some((key, text)) = png_text(kind == b"iTXt", body)
                    && let Some(key) = PNG_TEXT.iter().find(|k| **k == key)
                {
                    d.set(key, P::Str(text));
                }
            }
            b"IDAT" | b"IEND" => break,
            _ => {}
        }
        at = at.saturating_add(12 + len);
    }
    // An sRGB chunk implies sRGB's white point and primaries, which
    // ImageIO lists when no cHRM gives others.
    if d.get("sRGBIntent").is_some() && d.get("Chromaticities").is_none() {
        let srgb = [0.3127, 0.329, 0.64, 0.33, 0.3, 0.6, 0.15, 0.06];
        d.set("Chromaticities", P::Array(srgb.into_iter().map(P::Double).collect()));
    }
    d
}

/// The PNG text keywords ImageIO has keys for (the key is the keyword).
const PNG_TEXT: &[&str] = &[
    "Author",
    "Comment",
    "Copyright",
    "Creation Time",
    "Description",
    "Disclaimer",
    "Software",
    "Source",
    "Title",
    "Warning",
];

/// A `tEXt` chunk's keyword and Latin-1 text, or an uncompressed `iTXt`'s
/// keyword and UTF-8 text.
fn png_text(international: bool, body: &[u8]) -> Option<(String, String)> {
    let nul = body.iter().position(|&b| b == 0)?;
    let key: String = body[..nul].iter().map(|&b| char::from(b)).collect();
    let rest = &body[nul + 1..];
    let text = if international {
        // Compression flag and method, language tag, translated keyword.
        if *rest.first()? != 0 {
            return None;
        }
        let rest = rest.get(2..)?;
        let lang = rest.iter().position(|&b| b == 0)?;
        let rest = &rest[lang + 1..];
        let translated = rest.iter().position(|&b| b == 0)?;
        String::from_utf8_lossy(&rest[translated + 1..]).into_owned()
    } else {
        rest.iter().map(|&b| char::from(b)).collect()
    };
    Some((key, text))
}

/// A JPEG's `{JFIF}` from its APP0 segment, and the density it gives in
/// dots per inch.
fn jfif_dict(bytes: &[u8]) -> (Option<Dict>, Option<(f64, f64)>) {
    let mut progressive = false;
    let mut jfif = None;
    for (marker, body) in exif::jpeg_segments(bytes) {
        match marker {
            0xe0 if body.starts_with(b"JFIF\0") && body.len() >= 14 && jfif.is_none() => {
                let (major, minor, unit) = (body[5], body[6], body[7]);
                let x = u16::from_be_bytes([body[8], body[9]]);
                let y = u16::from_be_bytes([body[10], body[11]]);
                jfif = Some((major, minor, unit, x, y));
            }
            // Progressive frames: SOF2, SOF6, SOF10, SOF14.
            0xc2 | 0xc6 | 0xca | 0xce => progressive = true,
            _ => {}
        }
    }
    let Some((major, minor, unit, x, y)) = jfif else { return (None, None) };
    let mut d = Dict::new();
    d.set("DensityUnit", P::Int(i32::from(unit)));
    if progressive {
        d.set("IsProgressive", P::Bool(true));
    }
    // Version 1.02 is [1, 0, 2].
    let version = [major, minor / 10, minor % 10].map(|v| P::Int(i32::from(v)));
    d.set("JFIFVersion", P::Array(version.to_vec()));
    d.set("XDensity", P::Int(i32::from(x)));
    d.set("YDensity", P::Int(i32::from(y)));
    let dpi = match unit {
        1 if x > 0 && y > 0 => Some((f64::from(x), f64::from(y))),
        2 if x > 0 && y > 0 => Some((f64::from(x) * 2.54, f64::from(y) * 2.54)),
        _ => None,
    };
    (Some(d), dpi)
}

/// The density a TIFF directory's resolution tags give, in dots per inch:
/// as they are per inch, and in a TIFF file with no unit; times 2.54 per
/// centimeter.
fn tiff_dpi(ifd: &exif::Ifd, kind: Kind) -> Option<(f64, f64)> {
    let x = exif::get(ifd, 0x011a)?.number()?;
    let y = exif::get(ifd, 0x011b)?.number()?;
    let unit = exif::get(ifd, 0x0128).and_then(Value::number).unwrap_or(2.0) as u16;
    match unit {
        2 => Some((x, y)),
        3 => Some((x * 2.54, y * 2.54)),
        1 if kind == Kind::Tiff => Some((x, y)),
        _ => None,
    }
}

/// How a tag's value becomes a property.
#[derive(Clone, Copy)]
enum As {
    /// One number (an `int` or a `double` by the tag's type), or an array
    /// of them for more than one.
    Plain,
    /// Always an array.
    Array,
    /// Four ASCII digits, a version: "0232" is [2, 3, 2].
    Version,
    /// Text after an eight-byte character code (`UserComment`).
    Comment,
    /// One byte of an undefined value, as a number.
    Byte,
}

const TIFF_TAGS: &[(u16, &str, As)] = &[
    (0x0103, "Compression", As::Plain),
    (0x0106, "PhotometricInterpretation", As::Plain),
    (0x010d, "DocumentName", As::Plain),
    (0x010e, "ImageDescription", As::Plain),
    (0x010f, "Make", As::Plain),
    (0x0110, "Model", As::Plain),
    (0x0112, "Orientation", As::Plain),
    (0x011a, "XResolution", As::Plain),
    (0x011b, "YResolution", As::Plain),
    (0x011e, "XPosition", As::Plain),
    (0x011f, "YPosition", As::Plain),
    (0x0128, "ResolutionUnit", As::Plain),
    (0x012d, "TransferFunction", As::Array),
    (0x0131, "Software", As::Plain),
    (0x0132, "DateTime", As::Plain),
    (0x013b, "Artist", As::Plain),
    (0x013c, "HostComputer", As::Plain),
    (0x013e, "WhitePoint", As::Array),
    (0x013f, "PrimaryChromaticities", As::Array),
    (0x0142, "TileWidth", As::Plain),
    (0x0143, "TileLength", As::Plain),
    (0x8298, "Copyright", As::Plain),
];

const EXIF_TAGS: &[(u16, &str, As)] = &[
    (0x829a, "ExposureTime", As::Plain),
    (0x829d, "FNumber", As::Plain),
    (0x8822, "ExposureProgram", As::Plain),
    (0x8824, "SpectralSensitivity", As::Plain),
    (0x8827, "ISOSpeedRatings", As::Array),
    (0x8830, "SensitivityType", As::Plain),
    (0x8831, "StandardOutputSensitivity", As::Plain),
    (0x8832, "RecommendedExposureIndex", As::Plain),
    (0x8833, "ISOSpeed", As::Plain),
    (0x8834, "ISOSpeedLatitudeyyy", As::Plain),
    (0x8835, "ISOSpeedLatitudezzz", As::Plain),
    (0x9000, "ExifVersion", As::Version),
    (0x9003, "DateTimeOriginal", As::Plain),
    (0x9004, "DateTimeDigitized", As::Plain),
    (0x9010, "OffsetTime", As::Plain),
    (0x9011, "OffsetTimeOriginal", As::Plain),
    (0x9012, "OffsetTimeDigitized", As::Plain),
    (0x9101, "ComponentsConfiguration", As::Array),
    (0x9102, "CompressedBitsPerPixel", As::Plain),
    (0x9201, "ShutterSpeedValue", As::Plain),
    (0x9202, "ApertureValue", As::Plain),
    (0x9203, "BrightnessValue", As::Plain),
    (0x9204, "ExposureBiasValue", As::Plain),
    (0x9205, "MaxApertureValue", As::Plain),
    (0x9206, "SubjectDistance", As::Plain),
    (0x9207, "MeteringMode", As::Plain),
    (0x9208, "LightSource", As::Plain),
    (0x9209, "Flash", As::Plain),
    (0x920a, "FocalLength", As::Plain),
    (0x9214, "SubjectArea", As::Array),
    (0x9286, "UserComment", As::Comment),
    (0x9290, "SubsecTime", As::Plain),
    (0x9291, "SubsecTimeOriginal", As::Plain),
    (0x9292, "SubsecTimeDigitized", As::Plain),
    (0xa000, "FlashPixVersion", As::Version),
    (0xa001, "ColorSpace", As::Plain),
    (0xa002, "PixelXDimension", As::Plain),
    (0xa003, "PixelYDimension", As::Plain),
    (0xa004, "RelatedSoundFile", As::Plain),
    (0xa20b, "FlashEnergy", As::Plain),
    (0xa20e, "FocalPlaneXResolution", As::Plain),
    (0xa20f, "FocalPlaneYResolution", As::Plain),
    (0xa210, "FocalPlaneResolutionUnit", As::Plain),
    (0xa214, "SubjectLocation", As::Array),
    (0xa215, "ExposureIndex", As::Plain),
    (0xa217, "SensingMethod", As::Plain),
    (0xa300, "FileSource", As::Byte),
    (0xa301, "SceneType", As::Byte),
    (0xa401, "CustomRendered", As::Plain),
    (0xa402, "ExposureMode", As::Plain),
    (0xa403, "WhiteBalance", As::Plain),
    (0xa404, "DigitalZoomRatio", As::Plain),
    (0xa405, "FocalLenIn35mmFilm", As::Plain),
    (0xa406, "SceneCaptureType", As::Plain),
    (0xa407, "GainControl", As::Plain),
    (0xa408, "Contrast", As::Plain),
    (0xa409, "Saturation", As::Plain),
    (0xa40a, "Sharpness", As::Plain),
    (0xa40c, "SubjectDistRange", As::Plain),
    (0xa420, "ImageUniqueID", As::Plain),
    (0xa430, "CameraOwnerName", As::Plain),
    (0xa431, "BodySerialNumber", As::Plain),
    (0xa432, "LensSpecification", As::Array),
    (0xa433, "LensMake", As::Plain),
    (0xa434, "LensModel", As::Plain),
    (0xa435, "LensSerialNumber", As::Plain),
    (0xa460, "CompositeImage", As::Plain),
    (0xa461, "SourceImageNumberOfCompositeImage", As::Plain),
    (0xa462, "SourceExposureTimesOfCompositeImage", As::Array),
    (0xa500, "Gamma", As::Plain),
];

/// The properties of the tags of `ifd` that `table` names.
fn map_tags(ifd: &exif::Ifd, table: &[(u16, &'static str, As)]) -> Dict {
    let mut d = Dict::new();
    for (tag, value) in ifd {
        let Some(&(_, key, how)) = table.iter().find(|(t, ..)| t == tag) else { continue };
        if let Some(p) = property(value, how) {
            d.set(key, p);
        }
    }
    d
}

fn property(value: &Value, how: As) -> Option<P> {
    let number = |v: &Value, i: usize| match v {
        Value::Ints(n) => n.get(i).map(|&n| P::Int(n as i32)),
        Value::Reals(r) => r.get(i).map(|&r| P::Double(r)),
        Value::Bytes(b) => b.get(i).map(|&b| P::Int(i32::from(b))),
        Value::Ascii(_) => None,
    };
    let len = match value {
        Value::Ints(n) => n.len(),
        Value::Reals(r) => r.len(),
        Value::Bytes(b) => b.len(),
        Value::Ascii(_) => 1,
    };
    Some(match (how, value) {
        (As::Version, Value::Bytes(b)) => {
            let digits: Vec<i32> = b.iter().filter(|c| c.is_ascii_digit()).map(|&c| i32::from(c - b'0')).collect();
            if digits.len() < 3 {
                return None;
            }
            let major = digits[0] * 10 + digits[1];
            P::Array(std::iter::once(major).chain(digits[2..].iter().copied()).map(P::Int).collect())
        }
        (As::Comment, Value::Bytes(b)) => {
            let text = b.get(8..).unwrap_or_default();
            let text = text.split(|&c| c == 0).next().unwrap_or_default();
            P::Str(String::from_utf8_lossy(text).trim_end().to_owned())
        }
        (As::Comment, Value::Ascii(s)) => P::Str(s.clone()),
        (As::Byte, v) => number(v, 0)?,
        (_, Value::Ascii(s)) => P::Str(s.clone()),
        (As::Array, v) => P::Array((0..len).filter_map(|i| number(v, i)).collect()),
        (As::Plain, v) if len == 1 => number(v, 0)?,
        (As::Plain, v) if len > 1 => P::Array((0..len).filter_map(|i| number(v, i)).collect()),
        _ => return None,
    })
}

/// An ICC profile's description (its `desc` tag, as ICC version 2 or 4
/// writes it).
fn icc_description(icc: &[u8]) -> Option<String> {
    let be32 = |i: usize| icc.get(i..i + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let count = be32(128)?.min(1024);
    for i in 0..count {
        let entry = 132 + 12 * i;
        if icc.get(entry..entry + 4)? != b"desc" {
            continue;
        }
        let (at, len) = (be32(entry + 4)?, be32(entry + 8)?);
        let tag = icc.get(at..at.checked_add(len)?)?;
        return match tag.get(..4)? {
            // ASCII: a count (with the NUL) and the text.
            b"desc" => {
                let n = u32::from_be_bytes(tag.get(8..12)?.try_into().ok()?) as usize;
                let text = tag.get(12..12 + n)?;
                Some(String::from_utf8_lossy(text.split(|&b| b == 0).next()?).into_owned())
            }
            // Localized UTF-16: the first record.
            b"mluc" => {
                let size = u32::from_be_bytes(tag.get(20..24)?.try_into().ok()?) as usize;
                let offset = u32::from_be_bytes(tag.get(24..28)?.try_into().ok()?) as usize;
                let units: Vec<u16> =
                    tag.get(offset..offset + size)?.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
                Some(String::from_utf16_lossy(&units).trim_end_matches('\0').to_owned())
            }
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imageio::exif::tests::{Val, build};

    fn png(w: u32, h: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(w, h, image::Rgb([255, 0, 0]));
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
        out.into_inner()
    }

    /// `jpeg` with an EXIF block holding `tiff` after its start marker.
    fn with_exif(jpeg: &[u8], tiff: &[u8]) -> Vec<u8> {
        let mut segment = vec![0xff, 0xe1];
        segment.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
        segment.extend_from_slice(b"Exif\0\0");
        segment.extend_from_slice(tiff);
        [&jpeg[..2], &segment, &jpeg[2..]].concat()
    }

    #[test]
    fn a_png_is_described_as_imageio_describes_it() {
        let info = inspect(&png(4, 2)).expect("a PNG");
        assert_eq!((info.kind, info.count), (Kind::Png, 1));
        let p = &info.images[0].props;
        assert_eq!(p.get("PixelWidth"), Some(&P::Long(4)));
        assert_eq!(p.get("PixelHeight"), Some(&P::Long(2)));
        assert_eq!(p.get("Depth"), Some(&P::Long(8)));
        assert_eq!(p.get("ColorModel"), Some(&P::Str("RGB".into())));
        assert_eq!(p.get("HasAlpha"), Some(&P::Bool(true)));
        assert_eq!(p.get("ProfileName"), None);
        assert_eq!(p.get("{PNG}"), Some(&P::Dict(Dict(vec![("InterlaceType", P::Int(0))]))));
        assert!(info.file.is_empty());
    }

    #[test]
    fn jpegs_read_their_exif_and_thumbnail() {
        let thumb = jpeg(3, 2);
        let tiff = build(
            &[
                (0x0112, Val::Short(6)),
                (0x010f, Val::Ascii("Maker")),
                (0x011a, Val::Rational(300, 1)),
                (0x011b, Val::Rational(300, 1)),
                (0x0128, Val::Short(2)),
            ],
            &[(0x829d, Val::Rational(28, 10)), (0x8827, Val::Short(200))],
            Some(&thumb),
        );
        let file = with_exif(&jpeg(8, 4), &tiff);
        let info = inspect(&file).expect("a JPEG");
        let p = &info.images[0].props;
        assert_eq!(info.images[0].orientation, 6);
        assert_eq!(p.get("Orientation"), Some(&P::Int(6)));
        assert_eq!(p.get("DPIWidth"), Some(&P::Float(300.0)));
        assert_eq!(p.get("ProfileName"), Some(&P::Str(SRGB.into())));
        let Some(P::Dict(t)) = p.get("{TIFF}") else { panic!("a {{TIFF}}: {p:?}") };
        assert_eq!(t.get("Make"), Some(&P::Str("Maker".into())));
        assert_eq!(t.get("XResolution"), Some(&P::Double(300.0)));
        let Some(P::Dict(e)) = p.get("{Exif}") else { panic!("an {{Exif}}") };
        assert_eq!(e.get("FNumber"), Some(&P::Double(2.8)));
        assert_eq!(e.get("ISOSpeedRatings"), Some(&P::Array(vec![P::Int(200)])));
        let Some(P::Dict(j)) = p.get("{JFIF}") else { panic!("a {{JFIF}}") };
        assert_eq!(j.get("JFIFVersion"), Some(&P::Array(vec![P::Int(1), P::Int(0), P::Int(2)])));
        let range = info.thumbnail.expect("a thumbnail");
        assert_eq!(&file[range], &thumb[..]);
    }

    #[test]
    fn gifs_list_their_frames() {
        use image::codecs::gif::{GifEncoder, Repeat};
        let mut out = Vec::new();
        {
            let mut e = GifEncoder::new(&mut out);
            e.set_repeat(Repeat::Finite(2)).unwrap();
            let frames = [10u32, 0].map(|cs| {
                image::Frame::from_parts(
                    image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255])),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(cs * 10, 1),
                )
            });
            e.encode_frames(frames).unwrap();
        }
        let info = inspect(&out).expect("a GIF");
        assert_eq!(info.count, 2);
        let Some(P::Dict(g)) = info.file.get("{GIF}") else { panic!("{{GIF}}") };
        assert_eq!(g.get("LoopCount"), Some(&P::Int(3)));
        assert_eq!(g.get("CanvasPixelWidth"), Some(&P::Int(3)));
        let Some(P::Dict(f)) = info.images[1].props.get("{GIF}") else { panic!("a frame's {{GIF}}") };
        assert_eq!(f.get("DelayTime"), Some(&P::Double(0.1)));
        assert_eq!(f.get("UnclampedDelayTime"), Some(&P::Double(0.0)));
        // Cut short, the frames before the cut remain.
        let cut = inspect(&out[..out.len() - 20]).expect("a GIF still");
        assert!(cut.count >= 1);
    }

    #[test]
    fn unreadable_files_of_known_types_have_empty_images() {
        let info = inspect(b"\x89PNG\r\n\x1a\nnot really a PNG at all").expect("a PNG by its signature");
        assert_eq!((info.count, info.images.len()), (1, 0));
        let info = inspect(b"MM\0*\0\0\0\x08\0\0\0\0\0\0").expect("a TIFF by its signature");
        assert_eq!(info.count, 0);
        assert!(inspect(b"plain text").is_none());
        // Whatever the cut, nothing panics.
        let file = png(5, 5);
        for n in 0..file.len() {
            let _ = inspect(&file[..n]);
        }
    }

    /// `png` with a `tEXt` chunk of each of `texts` after its header.
    fn with_texts(png: &[u8], texts: &[(&str, &str)]) -> Vec<u8> {
        let crc = |bytes: &[u8]| {
            let mut c = !0u32;
            for &b in bytes {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        };
        let mut chunks = Vec::new();
        for (key, text) in texts {
            let body = [b"tEXt".as_slice(), key.as_bytes(), &[0], text.as_bytes()].concat();
            chunks.extend(((body.len() - 4) as u32).to_be_bytes());
            chunks.extend(&body);
            chunks.extend(crc(&body).to_be_bytes());
        }
        // The signature and the header chunk (13 bytes of data).
        let header = 8 + 8 + 13 + 4;
        [&png[..header], &chunks, &png[header..]].concat()
    }

    #[test]
    fn png_text_is_listed_where_imageio_lists_it() {
        let file =
            with_texts(&png(4, 2), &[("Title", "T"), ("Description", "D"), ("Comment", "C"), ("Copyright", "R")]);
        let info = inspect(&file).expect("a PNG");
        let p = &info.images[0].props;
        let dict = |key: &str| match p.get(key) {
            Some(P::Dict(d)) => d.clone(),
            other => panic!("{key}: {other:?}"),
        };
        assert_eq!(dict("{PNG}").get("Comment"), Some(&P::Str("C".into())));
        assert_eq!(dict("{Exif}").get("UserComment"), Some(&P::Str("C".into())));
        assert_eq!(dict("{TIFF}").get("Copyright"), Some(&P::Str("R".into())));
        let iptc = dict("{IPTC}");
        assert_eq!(iptc.get("ObjectName"), Some(&P::Str("T".into())));
        assert_eq!(iptc.get("Caption/Abstract"), Some(&P::Str("D".into())));
        // Without text, none of them.
        let info = inspect(&png(4, 2)).expect("a PNG");
        assert!(["{IPTC}", "{Exif}", "{TIFF}"].iter().all(|k| info.images[0].props.get(k).is_none()));
    }

    #[test]
    fn icc_descriptions_are_read() {
        let mut icc = vec![0u8; 128];
        icc.extend_from_slice(&1u32.to_be_bytes());
        icc.extend_from_slice(b"desc");
        icc.extend_from_slice(&144u32.to_be_bytes());
        icc.extend_from_slice(&22u32.to_be_bytes());
        icc.extend_from_slice(b"desc\0\0\0\0");
        icc.extend_from_slice(&10u32.to_be_bytes());
        icc.extend_from_slice(b"Display A\0");
        assert_eq!(icc_description(&icc).as_deref(), Some("Display A"));
        assert_eq!(icc_description(&icc[..140]), None);
    }
}
