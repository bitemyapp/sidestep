//! WTF-8: UTF-8 that may also hold surrogate code points, each as its own
//! three-byte sequence.
//!
//! Foundation defines strings as UTF-16 code units, and lets a string hold
//! half a surrogate pair: `substringToIndex:1` of an emoji keeps the high
//! surrogate. Sidestep stores text as UTF-8 so it can hand out `&str` and
//! `UTF8String` without transcoding, and WTF-8 is the smallest extension of
//! UTF-8 that can still represent every UTF-16 string exactly.
//!
//! The representation is canonical: a high surrogate is never immediately
//! followed by a low one, because such a pair is stored as the four-byte
//! sequence of the character it encodes. [`push`] and [`splice`] keep that
//! true when they join text, so equal UTF-16 strings always have equal bytes,
//! and `-isEqual:` and `-hash` can work on bytes.
//!
//! Strings whose [`HAS_SURROGATE`] flag is clear are valid UTF-8; everything
//! that hands text to Rust as `&str` checks it first.

use std::borrow::Cow;

/// Every byte is ASCII: UTF-16 indices are byte indices.
pub(crate) const ASCII: u8 = 1 << 0;
/// The text may hold a lone surrogate, so it is not valid UTF-8. Clear
/// means it certainly is valid UTF-8; set means it may not be.
pub(crate) const HAS_SURROGATE: u8 = 1 << 1;

/// The UTF-16 length of WTF-8 text: one unit per character, two for
/// characters outside the Basic Multilingual Plane.
#[inline]
pub(crate) fn utf16_len(bytes: &[u8]) -> usize {
    // Every byte that isn't a continuation byte starts a character, and a
    // four-byte lead adds the second unit of a pair. Written as a plain sum
    // so it vectorizes.
    bytes.iter().map(|&b| usize::from(b & 0xC0 != 0x80) + usize::from(b >= 0xF0)).sum()
}

/// The flags for `bytes`, scanning for surrogates only when `may_surrogate`.
pub(crate) fn flags_of(bytes: &[u8], may_surrogate: bool) -> u8 {
    if bytes.is_ascii() {
        return ASCII;
    }
    if may_surrogate && has_surrogate(bytes) { HAS_SURROGATE } else { 0 }
}

/// Whether WTF-8 text holds a surrogate code point (`ED A0..BF xx`).
pub(crate) fn has_surrogate(bytes: &[u8]) -> bool {
    memchr::memchr_iter(0xED, bytes).any(|i| bytes.get(i + 1).is_some_and(|&b| b >= 0xA0))
}

/// The length of the sequence a lead byte starts.
#[inline(always)]
pub(crate) fn width(lead: u8) -> usize {
    match lead {
        0x00..0x80 => 1,
        0x80..0xE0 => 2,
        0xE0..0xF0 => 3,
        _ => 4,
    }
}

/// Decode the code point starting at `at`, and its length in bytes. The
/// text must be WTF-8 and `at` a sequence boundary.
#[inline(always)]
pub(crate) fn decode(bytes: &[u8], at: usize) -> (u32, usize) {
    let b0 = u32::from(bytes[at]);
    match b0 {
        0x00..0x80 => (b0, 1),
        0x80..0xE0 => ((b0 & 0x1F) << 6 | u32::from(bytes[at + 1]) & 0x3F, 2),
        0xE0..0xF0 => ((b0 & 0x0F) << 12 | (u32::from(bytes[at + 1]) & 0x3F) << 6 | u32::from(bytes[at + 2]) & 0x3F, 3),
        _ => (
            (b0 & 0x07) << 18
                | (u32::from(bytes[at + 1]) & 0x3F) << 12
                | (u32::from(bytes[at + 2]) & 0x3F) << 6
                | u32::from(bytes[at + 3]) & 0x3F,
            4,
        ),
    }
}

/// The start of the sequence that ends just before `at`.
#[inline]
pub(crate) fn prev_boundary(bytes: &[u8], mut at: usize) -> usize {
    at -= 1;
    while bytes[at] & 0xC0 == 0x80 {
        at -= 1;
    }
    at
}

/// The two UTF-16 units of a supplementary code point.
#[inline(always)]
pub(crate) fn split_pair(c: u32) -> (u16, u16) {
    let c = c - 0x1_0000;
    (0xD800 | (c >> 10) as u16, 0xDC00 | (c & 0x3FF) as u16)
}

/// Encode a code point, surrogates included, as WTF-8.
#[inline]
pub(crate) fn encode(c: u32, out: &mut Vec<u8>) {
    match c {
        0..0x80 => out.push(c as u8),
        0x80..0x800 => out.extend_from_slice(&[0xC0 | (c >> 6) as u8, 0x80 | (c & 0x3F) as u8]),
        0x800..0x1_0000 => {
            out.extend_from_slice(&[0xE0 | (c >> 12) as u8, 0x80 | ((c >> 6) & 0x3F) as u8, 0x80 | (c & 0x3F) as u8])
        }
        _ => out.extend_from_slice(&[
            0xF0 | (c >> 18) as u8,
            0x80 | ((c >> 12) & 0x3F) as u8,
            0x80 | ((c >> 6) & 0x3F) as u8,
            0x80 | (c & 0x3F) as u8,
        ]),
    }
}

/// The high surrogate a WTF-8 buffer ends with, if any.
fn trailing_high(bytes: &[u8]) -> Option<u16> {
    match bytes {
        [.., 0xED, b1 @ 0xA0..=0xAF, b2] => Some(0xD000 | u16::from(b1 & 0x3F) << 6 | u16::from(b2 & 0x3F)),
        _ => None,
    }
}

/// The low surrogate WTF-8 text starts with, if any.
fn leading_low(bytes: &[u8]) -> Option<u16> {
    match bytes {
        [0xED, b1 @ 0xB0..=0xBF, b2, ..] => Some(0xD000 | u16::from(b1 & 0x3F) << 6 | u16::from(b2 & 0x3F)),
        _ => None,
    }
}

/// Append WTF-8 text, joining a high surrogate at the end of `out` with a
/// low one at the start of `bytes` into the character they encode.
pub(crate) fn push(out: &mut Vec<u8>, bytes: &[u8]) {
    if let (Some(high), Some(low)) = (trailing_high(out), leading_low(bytes)) {
        out.truncate(out.len() - 3);
        encode(0x1_0000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(low) - 0xDC00), out);
        out.extend_from_slice(&bytes[3..]);
    } else {
        out.extend_from_slice(bytes);
    }
}

/// Whether appending `b` to `a` joins a surrogate pair, so that the result
/// is not simply the two byte strings one after the other.
pub(crate) fn joins(a: &[u8], b: &[u8]) -> bool {
    trailing_high(a).is_some() && leading_low(b).is_some()
}

/// A position between UTF-16 units, as a byte offset into WTF-8 text. When
/// `low` is set the position falls inside the four-byte character at
/// `byte`, between its high and low surrogates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pos {
    pub byte: usize,
    pub low: bool,
}

impl Pos {
    pub(crate) const fn at(byte: usize) -> Pos {
        Pos { byte, low: false }
    }
}

/// The WTF-8 text between two positions, `start <= end`. A split
/// character contributes its surrogate on each side.
pub(crate) fn slice(bytes: &[u8], start: Pos, end: Pos) -> Cow<'_, [u8]> {
    if start == end {
        return Cow::Borrowed(&[]);
    }
    if !start.low && !end.low {
        return Cow::Borrowed(&bytes[start.byte..end.byte]);
    }
    // Both ends inside one character would mean start == end, handled
    // above, so a split start leaves at least its low surrogate.
    debug_assert!(!start.low || start.byte < end.byte);
    let mut out = Vec::with_capacity(end.byte - start.byte + 6);
    let mut from = start.byte;
    if start.low {
        let (c, w) = decode(bytes, start.byte);
        encode(u32::from(split_pair(c).1), &mut out);
        from += w;
    }
    out.extend_from_slice(&bytes[from..end.byte]);
    if end.low {
        let (c, _) = decode(bytes, end.byte);
        encode(u32::from(split_pair(c).0), &mut out);
    }
    Cow::Owned(out)
}

/// Replace the text between `start` and `end` with `insert`, keeping the
/// representation canonical: characters split by either position keep the
/// surrogate on their side, and surrogates that end up next to each other
/// join.
pub(crate) fn splice(buf: &mut Vec<u8>, start: Pos, end: Pos, insert: &[u8]) {
    // The text that replaces [start.byte, end_byte): the parts of split
    // characters that stay, and the insertion.
    let end_byte = if end.low { end.byte + 4 } else { end.byte };
    let joining = joins(&buf[..start.byte], insert)
        || joins(insert, &buf[end_byte..])
        || (insert.is_empty() && joins(&buf[..start.byte], &buf[end_byte..]));
    if !(start.low || end.low || joining) {
        buf.splice(start.byte..end_byte, insert.iter().copied());
        return;
    }
    let mut middle = Vec::with_capacity(insert.len() + 12);
    // Whatever precedes the edited region, so joins with it happen here.
    let keep_before = prev_char_start(buf, start.byte);
    middle.extend_from_slice(&buf[keep_before..start.byte]);
    if start.low {
        let (c, _) = decode(buf, start.byte);
        push(&mut middle, &utf8_of(u32::from(split_pair(c).0)));
    }
    push(&mut middle, insert);
    if end.low {
        let (c, _) = decode(buf, end.byte);
        push(&mut middle, &utf8_of(u32::from(split_pair(c).1)));
    }
    let keep_after = if end_byte < buf.len() { end_byte + width(buf[end_byte]) } else { end_byte };
    push(&mut middle, &buf[end_byte..keep_after]);
    buf.splice(keep_before..keep_after, middle);
}

/// The start of the character before `at`, or `at` itself at the start.
fn prev_char_start(bytes: &[u8], at: usize) -> usize {
    if at == 0 { 0 } else { prev_boundary(bytes, at) }
}

fn utf8_of(c: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(4);
    encode(c, &mut v);
    v
}

/// WTF-8 from UTF-16 units, unpaired surrogates kept.
pub(crate) fn from_utf16(units: impl IntoIterator<Item = u16>, capacity: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(capacity);
    let mut pending: Option<u16> = None;
    for u in units {
        if let Some(high) = pending.take() {
            if (0xDC00..0xE000).contains(&u) {
                encode(0x1_0000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(u) - 0xDC00), &mut out);
                continue;
            }
            encode(u32::from(high), &mut out);
        }
        if (0xD800..0xDC00).contains(&u) {
            pending = Some(u);
        } else {
            encode(u32::from(u), &mut out);
        }
    }
    if let Some(high) = pending {
        encode(u32::from(high), &mut out);
    }
    out
}

/// The text as `&str`, or `None` if it holds a surrogate.
#[inline]
pub(crate) fn as_str(bytes: &[u8], flags: u8) -> Option<&str> {
    if flags & HAS_SURROGATE != 0 && has_surrogate(bytes) {
        return None;
    }
    // SAFETY: WTF-8 without surrogate code points is UTF-8.
    Some(unsafe { std::str::from_utf8_unchecked(bytes) })
}

/// The text as UTF-8, with each lone surrogate replaced by U+FFFD.
pub(crate) fn to_str_lossy(bytes: &[u8], flags: u8) -> Cow<'_, str> {
    if let Some(s) = as_str(bytes, flags) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let (c, w) = decode(bytes, at);
        out.push(char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER));
        at += w;
    }
    Cow::Owned(out)
}

/// Iterate over the code points of WTF-8 text, surrogates included, with
/// their byte offsets.
pub(crate) fn code_points(bytes: &[u8]) -> impl Iterator<Item = (usize, u32)> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        if at >= bytes.len() {
            return None;
        }
        let (c, w) = decode(bytes, at);
        let item = (at, c);
        at += w;
        Some(item)
    })
}

/// Iterate over the UTF-16 units of WTF-8 text.
pub(crate) fn units(bytes: &[u8]) -> impl Iterator<Item = u16> + '_ {
    let mut at = 0;
    let mut low: Option<u16> = None;
    std::iter::from_fn(move || {
        if let Some(u) = low.take() {
            return Some(u);
        }
        if at >= bytes.len() {
            return None;
        }
        let (c, w) = decode(bytes, at);
        at += w;
        if c >= 0x1_0000 {
            let (h, l) = split_pair(c);
            low = Some(l);
            Some(h)
        } else {
            Some(c as u16)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wtf8(units: &[u16]) -> Vec<u8> {
        from_utf16(units.iter().copied(), 0)
    }

    #[test]
    fn utf16_lengths() {
        assert_eq!(utf16_len(b""), 0);
        assert_eq!(utf16_len("a🎉é漢".as_bytes()), 5);
        assert_eq!(utf16_len(&wtf8(&[0xD83C])), 1);
    }

    #[test]
    fn surrogates_round_trip() {
        let pair = "🎉".encode_utf16().collect::<Vec<_>>();
        assert_eq!(wtf8(&pair), "🎉".as_bytes());
        let lone = wtf8(&[0x61, 0xD83C, 0x62]);
        assert_eq!(lone.len(), 5);
        assert!(has_surrogate(&lone));
        assert_eq!(units(&lone).collect::<Vec<_>>(), [0x61, 0xD83C, 0x62]);
        assert!(!has_surrogate("é漢🎉".as_bytes()));
    }

    #[test]
    fn push_joins_pairs() {
        let mut hi = wtf8(&[0xD83C]);
        push(&mut hi, &wtf8(&[0xDF89, 0x61]));
        assert_eq!(hi, "🎉a".as_bytes());
    }

    #[test]
    fn slices_split_pairs() {
        let s = "a🎉b".as_bytes();
        let mid = Pos { byte: 1, low: true };
        assert_eq!(&*slice(s, Pos::at(0), mid), &wtf8(&[0x61, 0xD83C])[..]);
        assert_eq!(&*slice(s, mid, Pos::at(6)), &wtf8(&[0xDF89, 0x62])[..]);
        assert_eq!(&*slice(s, mid, mid), b"");
    }

    #[test]
    fn splices_stay_canonical() {
        // Delete the high half of a pair.
        let mut s = "🎉".as_bytes().to_vec();
        splice(&mut s, Pos::at(0), Pos { byte: 0, low: true }, b"");
        assert_eq!(s, wtf8(&[0xDF89]));
        // Put it back.
        splice(&mut s, Pos::at(0), Pos::at(0), &wtf8(&[0xD83C]));
        assert_eq!(s, "🎉".as_bytes());
        // Delete what separates two halves.
        let mut s = wtf8(&[0xD83C, 0x78, 0xDF89]);
        splice(&mut s, Pos::at(3), Pos::at(4), b"");
        assert_eq!(s, "🎉".as_bytes());
        // Replace inside a pair.
        let mut s = "a🎉b".as_bytes().to_vec();
        splice(&mut s, Pos { byte: 1, low: true }, Pos { byte: 1, low: true }, b"x");
        assert_eq!(s, wtf8(&[0x61, 0xD83C, 0x78, 0xDF89, 0x62]));
    }
}
