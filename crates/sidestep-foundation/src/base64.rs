//! Base64 as `NSData` speaks it (RFC 4648's standard alphabet).
//!
//! Encoding can break lines every 64 or 76 characters (asking for both
//! means neither), ending them with CR, LF or, by default, both; there is
//! no line ending after the last line. Decoding wants complete padding;
//! further `=` after it are tolerated, and bits left over in the last
//! character are ignored. With `IgnoreUnknownCharacters` anything outside
//! the alphabet is skipped, but padding must then be exactly what the data
//! needs. These rules are what macOS does (see
//! `conformance/tests/services.rs`).

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) const LINE_64: usize = 1 << 0;
pub(crate) const LINE_76: usize = 1 << 1;
pub(crate) const END_CR: usize = 1 << 4;
pub(crate) const END_LF: usize = 1 << 5;

pub(crate) const IGNORE_UNKNOWN: usize = 1 << 0;

pub(crate) fn encode(bytes: &[u8], options: usize) -> String {
    let line = match options & (LINE_64 | LINE_76) {
        LINE_64 => 64,
        LINE_76 => 76,
        _ => usize::MAX,
    };
    let ending = match options & (END_CR | END_LF) {
        END_CR => "\r",
        END_LF => "\n",
        _ => "\r\n",
    };
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut column = 0;
    let mut push = |c: u8, out: &mut String| {
        if column == line {
            out.push_str(ending);
            column = 0;
        }
        out.push(c as char);
        column += 1;
    };
    for chunk in bytes.chunks(3) {
        let n = u32::from(chunk[0]) << 16
            | u32::from(chunk.get(1).copied().unwrap_or(0)) << 8
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for i in 0..4 {
            let c = if i <= chunk.len() { ALPHABET[(n >> (18 - 6 * i)) as usize & 63] } else { b'=' };
            push(c, &mut out);
        }
    }
    out
}

fn value(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    } as u32)
}

pub(crate) fn decode(text: &[u8], options: usize) -> Option<Vec<u8>> {
    let ignore = options & IGNORE_UNKNOWN != 0;
    let mut digits = Vec::with_capacity(text.len());
    let mut pads = 0;
    for &c in text {
        match value(c) {
            // Data after padding is malformed.
            Some(_) if pads > 0 => return None,
            Some(v) => digits.push(v),
            None if c == b'=' => pads += 1,
            None if ignore => {}
            None => return None,
        }
    }
    let needed = match digits.len() % 4 {
        0 => 0,
        1 => return None,
        2 => 2,
        _ => 1,
    };
    let pads_ok = if ignore { pads == needed } else { pads >= needed && (needed > 0 || pads == 0) };
    if !pads_ok {
        return None;
    }
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for chunk in digits.chunks(4) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &v)| n | v << (18 - 6 * i));
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..chunk.len() - 1]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len as u8).map(|b| b.wrapping_mul(37)).collect();
            for options in [0, LINE_64, LINE_76 | END_LF] {
                let text = encode(&bytes, options);
                assert_eq!(decode(text.as_bytes(), IGNORE_UNKNOWN).unwrap(), bytes);
            }
            assert_eq!(decode(encode(&bytes, 0).as_bytes(), 0).unwrap(), bytes);
        }
    }
}
