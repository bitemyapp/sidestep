//! `NSStringEncoding`: reading bytes into WTF-8 and writing text out.
//!
//! Supported: ASCII, UTF-8, ISO Latin 1, Mac OS Roman, Windows-1252, and
//! UTF-16 and UTF-32 in all three byte-order forms. Anything else fails the
//! way an unconvertible string does (nil, 0 or NO). Behaviour matches what
//! the conformance tests observe on macOS: a UTF-8 or unmarked UTF-16/32
//! byte order mark is dropped, UTF-16 and UTF-32 without one are big-endian,
//! and ASCII input accepts any byte, reading it as Latin 1.

use std::borrow::Cow;

use objc2_foundation::NSStringEncoding;

use super::index::Text;
use super::wtf8::{self, ASCII, HAS_SURROGATE};

pub(crate) const ASCII_ENC: u32 = 1;
pub(crate) const UTF8: u32 = 4;
pub(crate) const LATIN1: u32 = 5;
pub(crate) const WINDOWS_1252: u32 = 12;
pub(crate) const UTF16: u32 = 10;
pub(crate) const MAC_ROMAN: u32 = 30;
pub(crate) const UTF16_BE: u32 = 0x9000_0100;
pub(crate) const UTF16_LE: u32 = 0x9400_0100;
pub(crate) const UTF32: u32 = 0x8c00_0100;
pub(crate) const UTF32_BE: u32 = 0x9800_0100;
pub(crate) const UTF32_LE: u32 = 0x9c00_0100;

/// `NSStringEncodingConversionAllowLossy`.
pub(crate) const ALLOW_LOSSY: usize = 1;
/// `NSStringEncodingConversionExternalRepresentation`: add a byte order mark.
pub(crate) const EXTERNAL: usize = 2;

/// An `NSStringEncoding` as Sidestep compares it. On GNUstep it is an `int`
/// (objc2's fork, see docs/abi.md), and the encodings above 0x7fffffff
/// arrive as negative numbers with the same bits.
#[inline]
pub(crate) fn arg(raw: NSStringEncoding) -> u32 {
    raw as u32
}

/// The `NSStringEncoding` for one of the constants above.
#[inline]
pub(crate) fn raw(encoding: u32) -> NSStringEncoding {
    encoding as NSStringEncoding
}

/// Text decoded from bytes: WTF-8, its UTF-16 length and its flags.
pub(crate) struct Decoded<'a> {
    pub bytes: Cow<'a, [u8]>,
    pub utf16_len: usize,
    pub flags: u8,
}

/// UTF-8 bytes, validated, with a leading byte order mark dropped.
pub(crate) fn decode_utf8(bytes: &[u8]) -> Option<Decoded<'_>> {
    if bytes.is_ascii() {
        return Some(Decoded { bytes: Cow::Borrowed(bytes), utf16_len: bytes.len(), flags: ASCII });
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    std::str::from_utf8(bytes).ok()?;
    let flags = if bytes.is_ascii() { ASCII } else { 0 };
    Some(Decoded { bytes: Cow::Borrowed(bytes), utf16_len: wtf8::utf16_len(bytes), flags })
}

/// Bytes in `encoding` as WTF-8, or `None` if they aren't valid in it or the
/// encoding isn't supported.
pub(crate) fn decode(bytes: &[u8], encoding: u32) -> Option<Decoded<'_>> {
    match encoding {
        UTF8 => decode_utf8(bytes),
        ASCII_ENC | LATIN1 => Some(single_byte(bytes, |b| Some(u32::from(b)))),
        MAC_ROMAN => Some(single_byte(bytes, |b| Some(u32::from(MAC_ROMAN_HIGH[usize::from(b - 0x80)])))),
        WINDOWS_1252 => {
            let mut bad = false;
            let d = single_byte(bytes, |b| {
                let c = cp1252(b);
                bad |= c.is_none();
                c
            });
            (!bad).then_some(d)
        }
        UTF16 | UTF16_BE | UTF16_LE => {
            let (bytes, big) = match (encoding, bytes) {
                (UTF16, [0xFE, 0xFF, rest @ ..]) => (rest, true),
                (UTF16, [0xFF, 0xFE, rest @ ..]) => (rest, false),
                (UTF16_LE, _) => (bytes, false),
                _ => (bytes, true),
            };
            // A trailing odd byte is ignored, as on macOS.
            let pairs = bytes.as_chunks::<2>().0;
            let units = pairs.iter().map(|&p| if big { u16::from_be_bytes(p) } else { u16::from_le_bytes(p) });
            Some(from_units(units, pairs.len()))
        }
        UTF32 | UTF32_BE | UTF32_LE => {
            let (bytes, big) = match (encoding, bytes) {
                (UTF32, [0, 0, 0xFE, 0xFF, rest @ ..]) => (rest, true),
                (UTF32, [0xFF, 0xFE, 0, 0, rest @ ..]) => (rest, false),
                (UTF32_LE, _) => (bytes, false),
                _ => (bytes, true),
            };
            let quads = bytes.as_chunks::<4>().0;
            let mut out = Vec::with_capacity(quads.len());
            let mut utf16_len = 0;
            for &q in quads {
                let c = if big { u32::from_be_bytes(q) } else { u32::from_le_bytes(q) };
                // Surrogates and values past U+10FFFF are invalid in UTF-32.
                let c = char::from_u32(c)?;
                utf16_len += c.len_utf16();
                wtf8::encode(u32::from(c), &mut out);
            }
            let flags = if out.is_ascii() { ASCII } else { 0 };
            Some(Decoded { bytes: Cow::Owned(out), utf16_len, flags })
        }
        _ => None,
    }
}

/// Decode a single-byte encoding whose lower half is ASCII.
fn single_byte(bytes: &[u8], mut high: impl FnMut(u8) -> Option<u32>) -> Decoded<'_> {
    if bytes.is_ascii() {
        return Decoded { bytes: Cow::Borrowed(bytes), utf16_len: bytes.len(), flags: ASCII };
    }
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 2);
    for &b in bytes {
        if b < 0x80 {
            out.push(b);
        } else if let Some(c) = high(b) {
            wtf8::encode(c, &mut out);
        }
    }
    // Every byte is one BMP character.
    Decoded { bytes: Cow::Owned(out), utf16_len: bytes.len(), flags: 0 }
}

/// WTF-8 from UTF-16 units, which may include unpaired surrogates.
pub(crate) fn from_units(units: impl IntoIterator<Item = u16>, count: usize) -> Decoded<'static> {
    let out = wtf8::from_utf16(units, count + count / 2);
    let flags = wtf8::flags_of(&out, true);
    Decoded { bytes: Cow::Owned(out), utf16_len: count, flags }
}

/// Characters 0x80 to 0xFF of Mac OS Roman.
const MAC_ROMAN_HIGH: [u16; 128] = [
    0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1, 0x00E0, 0x00E2, 0x00E4, 0x00E3, 0x00E5, 0x00E7,
    0x00E9, 0x00E8, 0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF, 0x00F1, 0x00F3, 0x00F2, 0x00F4, 0x00F6, 0x00F5,
    0x00FA, 0x00F9, 0x00FB, 0x00FC, 0x2020, 0x00B0, 0x00A2, 0x00A3, 0x00A7, 0x2022, 0x00B6, 0x00DF, 0x00AE, 0x00A9,
    0x2122, 0x00B4, 0x00A8, 0x2260, 0x00C6, 0x00D8, 0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202, 0x2211,
    0x220F, 0x03C0, 0x222B, 0x00AA, 0x00BA, 0x03A9, 0x00E6, 0x00F8, 0x00BF, 0x00A1, 0x00AC, 0x221A, 0x0192, 0x2248,
    0x2206, 0x00AB, 0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3, 0x00D5, 0x0152, 0x0153, 0x2013, 0x2014, 0x201C, 0x201D,
    0x2018, 0x2019, 0x00F7, 0x25CA, 0x00FF, 0x0178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02, 0x2021, 0x00B7,
    0x201A, 0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1, 0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF, 0x00CC, 0x00D3, 0x00D4,
    0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC, 0x00AF, 0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD,
    0x02DB, 0x02C7,
];

/// Characters 0x80 to 0x9F of Windows-1252; 0 marks the five it leaves
/// undefined. The rest of the upper half is Latin 1.
const CP1252_C1: [u16; 32] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0, 0,
    0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
];

fn cp1252(b: u8) -> Option<u32> {
    match b {
        0x80..0xA0 => match CP1252_C1[usize::from(b - 0x80)] {
            0 => None,
            c => Some(u32::from(c)),
        },
        _ => Some(u32::from(b)),
    }
}

/// The byte a single-byte encoding writes for code point `c`.
fn single_byte_of(c: u32, encoding: u32) -> Option<u8> {
    if c < 0x80 {
        return Some(c as u8);
    }
    match encoding {
        ASCII_ENC => None,
        LATIN1 => (c < 0x100).then_some(c as u8),
        MAC_ROMAN => MAC_ROMAN_HIGH.iter().position(|&m| u32::from(m) == c).map(|i| 0x80 + i as u8),
        WINDOWS_1252 => match c {
            0xA0..0x100 => Some(c as u8),
            _ => CP1252_C1.iter().position(|&m| m != 0 && u32::from(m) == c).map(|i| 0x80 + i as u8),
        },
        _ => None,
    }
}

fn is_single_byte(encoding: u32) -> bool {
    matches!(encoding, ASCII_ENC | LATIN1 | MAC_ROMAN | WINDOWS_1252)
}

/// Whether Sidestep can convert to and from `encoding`.
pub(crate) fn supported(encoding: u32) -> bool {
    is_single_byte(encoding) || matches!(encoding, UTF8 | UTF16 | UTF16_BE | UTF16_LE | UTF32 | UTF32_BE | UTF32_LE)
}

/// `-lengthOfBytesUsingEncoding:` over any text: 0 when it can't be
/// encoded without loss.
pub(crate) fn byte_length(text: &Text, encoding: u32) -> usize {
    let bytes = text.bytes;
    match encoding {
        UTF8 if text.flags & HAS_SURROGATE == 0 || !wtf8::has_surrogate(bytes) => bytes.len(),
        UTF8 => 0,
        UTF16 | UTF16_BE | UTF16_LE => 2 * text.utf16_len,
        UTF32 | UTF32_BE | UTF32_LE => {
            if text.flags & HAS_SURROGATE != 0 && wtf8::has_surrogate(bytes) {
                0
            } else {
                4 * bytes.iter().filter(|&&b| b & 0xC0 != 0x80).count()
            }
        }
        _ if text.is_ascii() && is_single_byte(encoding) => bytes.len(),
        _ if is_single_byte(encoding) => {
            let mut n = 0;
            for (_, c) in wtf8::code_points(bytes) {
                if single_byte_of(c, encoding).is_none() {
                    return 0;
                }
                n += 1;
            }
            n
        }
        _ => 0,
    }
}

/// `-maximumLengthOfBytesUsingEncoding:`.
pub(crate) fn max_byte_length(utf16_len: usize, encoding: u32) -> usize {
    match encoding {
        UTF8 => 3 * utf16_len,
        UTF16 | UTF16_BE | UTF16_LE => 2 * utf16_len,
        UTF32 | UTF32_BE | UTF32_LE => 4 * utf16_len,
        _ if is_single_byte(encoding) => utf16_len,
        _ => 0,
    }
}

/// `-canBeConvertedToEncoding:`. As on macOS, UTF-8 and UTF-16 accept a
/// lone surrogate and UTF-32 doesn't.
pub(crate) fn can_convert(text: &Text, encoding: u32) -> bool {
    match encoding {
        UTF8 | UTF16 | UTF16_BE | UTF16_LE => true,
        UTF32 | UTF32_BE | UTF32_LE => text.flags & HAS_SURROGATE == 0 || !wtf8::has_surrogate(text.bytes),
        _ if is_single_byte(encoding) => {
            text.is_ascii() || wtf8::code_points(text.bytes).all(|(_, c)| single_byte_of(c, encoding).is_some())
        }
        _ => false,
    }
}

/// The encoding `-fastestEncoding` reports: ASCII when the text is, else
/// UTF-16.
pub(crate) fn fastest(text: &Text) -> u32 {
    if text.is_ascii() || text.bytes.is_ascii() { ASCII_ENC } else { UTF16 }
}

/// The encoding `-smallestEncoding` reports: ASCII, then Mac OS Roman,
/// then UTF-16, as macOS picks them.
pub(crate) fn smallest(text: &Text) -> u32 {
    if text.is_ascii() || text.bytes.is_ascii() {
        ASCII_ENC
    } else if can_convert(text, MAC_ROMAN) {
        MAC_ROMAN
    } else {
        UTF16
    }
}

/// How many bytes a NUL terminator takes in `encoding`.
pub(crate) fn nul_width(encoding: u32) -> usize {
    match encoding {
        UTF16 | UTF16_BE | UTF16_LE => 2,
        UTF32 | UTF32_BE | UTF32_LE => 4,
        _ => 1,
    }
}

/// The result of [`encode_units`]: bytes written and where it stopped.
pub(crate) struct Encoded {
    pub used: usize,
    /// UTF-16 units consumed from the start of the range.
    pub consumed: usize,
}

/// Encode the UTF-16 range `loc..loc + len` of `text` into `out` (or just
/// count, when `out` is `None`), stopping at `max` bytes or at the first
/// character that can't be encoded, unless `lossy`. This is the loop behind
/// `-getBytes:maxLength:usedLength:encoding:options:range:remainingRange:`.
pub(crate) fn encode_units(
    text: &Text,
    loc: usize,
    len: usize,
    encoding: u32,
    options: usize,
    max: usize,
    mut out: Option<&mut [u8]>,
) -> Encoded {
    let limit = if out.is_some() { max } else { usize::MAX };
    let mut used = 0;
    let mut put = |bytes: &[u8], out: &mut Option<&mut [u8]>| -> bool {
        if used + bytes.len() > limit {
            return false;
        }
        if let Some(out) = out {
            out[used..used + bytes.len()].copy_from_slice(bytes);
        }
        used += bytes.len();
        true
    };
    let lossy = options & ALLOW_LOSSY != 0;
    let host_little = cfg!(target_endian = "little");
    // Unmarked UTF-16 and UTF-32 are written in host order here, with a
    // byte order mark only when asked for.
    let (utf16_little, utf32_little) = (
        match encoding {
            UTF16_BE => false,
            UTF16_LE => true,
            _ => host_little,
        },
        match encoding {
            UTF32_BE => false,
            UTF32_LE => true,
            _ => host_little,
        },
    );
    if options & EXTERNAL != 0 && len > 0 {
        let bom: &[u8] = match encoding {
            UTF16 if utf16_little => &[0xFF, 0xFE],
            UTF16 => &[0xFE, 0xFF],
            UTF32 if utf32_little => &[0xFF, 0xFE, 0, 0],
            UTF32 => &[0, 0, 0xFE, 0xFF],
            _ => &[],
        };
        if !put(bom, &mut out) {
            return Encoded { used: 0, consumed: 0 };
        }
    }
    let mut units = [0u16; 2];
    let mut consumed = 0;
    let mut scratch = Vec::with_capacity(4);
    while consumed < len {
        // Take one character: a pair when both halves are in range.
        text.copy_units(loc + consumed, 1, &mut units[..1]);
        let mut n = 1;
        let mut c = u32::from(units[0]);
        if (0xD800..0xDC00).contains(&units[0]) && consumed + 1 < len {
            text.copy_units(loc + consumed + 1, 1, &mut units[1..]);
            if (0xDC00..0xE000).contains(&units[1]) {
                n = 2;
                c = 0x1_0000 + ((c - 0xD800) << 10) + (u32::from(units[1]) - 0xDC00);
            }
        }
        let lone = (0xD800..0xE000).contains(&c);
        scratch.clear();
        let ok = match encoding {
            UTF8 if !lone => {
                wtf8::encode(c, &mut scratch);
                true
            }
            UTF16 | UTF16_BE | UTF16_LE => {
                for &u in &units[..n] {
                    scratch.extend_from_slice(&if utf16_little { u.to_le_bytes() } else { u.to_be_bytes() });
                }
                true
            }
            UTF32 | UTF32_BE | UTF32_LE if !lone => {
                scratch.extend_from_slice(&if utf32_little { c.to_le_bytes() } else { c.to_be_bytes() });
                true
            }
            _ if is_single_byte(encoding) => match single_byte_of(c, encoding) {
                Some(b) => {
                    scratch.push(b);
                    true
                }
                None if lossy => {
                    scratch.push(lossy_byte(c, encoding));
                    true
                }
                None => false,
            },
            _ => false,
        };
        if !ok || !put(&scratch, &mut out) {
            break;
        }
        consumed += n;
    }
    Encoded { used, consumed }
}

/// What a lossy conversion writes for a character the encoding lacks: its
/// base letter when it decomposes into one the encoding has, else `?`.
fn lossy_byte(c: u32, encoding: u32) -> u8 {
    super::fold::base_letter(c).and_then(|base| single_byte_of(base, encoding)).unwrap_or(b'?')
}

/// `-dataUsingEncoding:allowLossyConversion:`: the whole text in
/// `encoding`, after a byte order mark for unmarked UTF-16 and UTF-32 (even
/// when the text is empty, as on macOS), or `None` if some character can't
/// be written (and `lossy` is off) or the encoding isn't supported.
pub(crate) fn external_representation(text: &Text, encoding: u32, lossy: bool) -> Option<Vec<u8>> {
    if !supported(encoding) {
        return None;
    }
    let body = encode_all(text, encoding, lossy)?;
    let mut out = match encoding {
        UTF16 => {
            if cfg!(target_endian = "little") { 0xFEFFu16.to_le_bytes() } else { 0xFEFFu16.to_be_bytes() }.to_vec()
        }
        UTF32 => {
            if cfg!(target_endian = "little") { 0xFEFFu32.to_le_bytes() } else { 0xFEFFu32.to_be_bytes() }.to_vec()
        }
        _ => Vec::new(),
    };
    out.extend(body);
    Some(out)
}

/// The whole text in `encoding`, or `None` if some character can't be
/// written (and `lossy` is off).
pub(crate) fn encode_all(text: &Text, encoding: u32, lossy: bool) -> Option<Vec<u8>> {
    if encoding == UTF8 || (text.is_ascii() && is_single_byte(encoding)) {
        return (byte_length(text, UTF8) == text.bytes.len() || text.bytes.is_empty()).then(|| text.bytes.to_vec());
    }
    let max = max_byte_length(text.utf16_len, encoding);
    let mut out = vec![0; max];
    let options = if lossy { ALLOW_LOSSY } else { 0 };
    let done = encode_units(text, 0, text.utf16_len, encoding, options, max, Some(&mut out));
    if done.consumed < text.utf16_len {
        return None;
    }
    out.truncate(done.used);
    Some(out)
}
