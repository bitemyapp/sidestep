//! TIFF structures: the image file directories (IFDs) of a TIFF file, and
//! of the EXIF block a JPEG (APP1), PNG (`eXIf`) or WebP (`EXIF`) file
//! carries, which is a TIFF structure too. Written from the TIFF 6.0 and
//! EXIF 2.3 specifications: a byte order mark, then chains of directories
//! of 12-byte entries (tag, type, count, and the value or its offset).
//!
//! Reading never trusts an offset or a count: every read is bounds-checked,
//! each directory is visited once, and directories and entries are
//! capped, so a malformed file reads as fewer tags, never as a panic or a
//! hang.

/// A tag's value, by its TIFF type.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    /// BYTE, SBYTE and UNDEFINED.
    Bytes(Vec<u8>),
    Ascii(String),
    /// SHORT, LONG, SSHORT and SLONG, widened.
    Ints(Vec<i64>),
    /// RATIONAL and SRATIONAL, FLOAT and DOUBLE.
    Reals(Vec<f64>),
}

impl Value {
    /// The first number, integer or real.
    pub fn number(&self) -> Option<f64> {
        match self {
            Value::Ints(v) => v.first().map(|&n| n as f64),
            Value::Reals(v) => v.first().copied(),
            Value::Bytes(v) => v.first().map(|&n| f64::from(n)),
            Value::Ascii(_) => None,
        }
    }
}

/// One directory's entries, in file order.
pub(crate) type Ifd = Vec<(u16, Value)>;

/// The directories of a TIFF structure that ImageIO reports on.
#[derive(Debug, Default)]
pub(crate) struct Tiff {
    /// The first image's directory (IFD0).
    pub ifd0: Ifd,
    /// The EXIF sub-directory IFD0 points at (tag 0x8769).
    pub exif: Ifd,
    /// The next directory in IFD0's chain: in EXIF, the thumbnail's (IFD1).
    pub ifd1: Ifd,
    /// How many directories IFD0's chain has (in a TIFF file, one a page).
    pub pages: usize,
}

/// The tag of IFD0's pointer to the EXIF directory.
const EXIF_POINTER: u16 = 0x8769;
/// The tags of a JPEG thumbnail's offset and length (IFD1).
const THUMB_OFFSET: u16 = 0x0201;
const THUMB_LENGTH: u16 = 0x0202;
/// The most directories and entries read.
const MAX_IFDS: usize = 64;
const MAX_ENTRIES: usize = 1024;

struct Reader<'a> {
    data: &'a [u8],
    little: bool,
}

impl Reader<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }

    fn u64(&self, at: usize) -> Option<u64> {
        let b: [u8; 8] = self.data.get(at..at.checked_add(8)?)?.try_into().ok()?;
        Some(if self.little { u64::from_le_bytes(b) } else { u64::from_be_bytes(b) })
    }

    /// The directory at `at`: its entries and the offset of the next one.
    fn ifd(&self, at: usize) -> Option<(Ifd, usize)> {
        let count = usize::from(self.u16(at)?).min(MAX_ENTRIES);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let entry = at + 2 + 12 * i;
            let (Some(tag), Some(kind), Some(n)) = (self.u16(entry), self.u16(entry + 2), self.u32(entry + 4)) else {
                break;
            };
            if let Some(value) = self.value(kind, n as usize, entry + 8) {
                out.push((tag, value));
            }
        }
        let next = self.u32(at + 2 + 12 * count).unwrap_or(0) as usize;
        Some((out, next))
    }

    /// An entry's value: `n` items of type `kind`, in the entry's last four
    /// bytes if they fit, else at the offset they hold.
    fn value(&self, kind: u16, n: usize, field: usize) -> Option<Value> {
        let size = match kind {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => return None,
        };
        let len = n.checked_mul(size)?;
        let at = if len <= 4 { field } else { self.u32(field)? as usize };
        let bytes = self.data.get(at..at.checked_add(len)?)?;
        let item = |i: usize| at + i * size;
        Some(match kind {
            1 | 6 | 7 => Value::Bytes(bytes.to_vec()),
            2 => {
                let text = bytes.split(|&b| b == 0).next().unwrap_or_default();
                Value::Ascii(String::from_utf8_lossy(text).into_owned())
            }
            3 => Value::Ints((0..n).filter_map(|i| self.u16(item(i)).map(i64::from)).collect()),
            8 => Value::Ints((0..n).filter_map(|i| self.u16(item(i)).map(|v| i64::from(v as i16))).collect()),
            4 => Value::Ints((0..n).filter_map(|i| self.u32(item(i)).map(i64::from)).collect()),
            9 => Value::Ints((0..n).filter_map(|i| self.u32(item(i)).map(|v| i64::from(v as i32))).collect()),
            5 | 10 => Value::Reals(
                (0..n)
                    .filter_map(|i| {
                        let (a, b) = (self.u32(item(i))?, self.u32(item(i) + 4)?);
                        let (a, b) = if kind == 10 {
                            (f64::from(a as i32), f64::from(b as i32))
                        } else {
                            (f64::from(a), f64::from(b))
                        };
                        Some(if b == 0.0 { 0.0 } else { a / b })
                    })
                    .collect(),
            ),
            11 => {
                Value::Reals((0..n).filter_map(|i| self.u32(item(i)).map(|v| f64::from(f32::from_bits(v)))).collect())
            }
            12 => Value::Reals((0..n).filter_map(|i| self.u64(item(i)).map(f64::from_bits)).collect()),
            _ => return None,
        })
    }
}

/// The directories of the TIFF structure `data` holds (a TIFF file, or an
/// EXIF block without its `Exif\0\0` prefix), or `None` if it isn't one.
pub(crate) fn parse(data: &[u8]) -> Option<Tiff> {
    let little = match data.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let r = Reader { data, little };
    let mut tiff = Tiff::default();
    let mut seen = Vec::new();
    let mut at = r.u32(4)? as usize;
    while at != 0 && seen.len() < MAX_IFDS && !seen.contains(&at) {
        seen.push(at);
        let Some((ifd, next)) = r.ifd(at) else { break };
        match seen.len() {
            1 => tiff.ifd0 = ifd,
            2 => tiff.ifd1 = ifd,
            _ => {}
        }
        at = next;
    }
    tiff.pages = seen.len();
    if let Some(Value::Ints(p)) = get(&tiff.ifd0, EXIF_POINTER)
        && let Some(&p) = p.first()
        && let Ok(p) = usize::try_from(p)
        && !seen.contains(&p)
        && let Some((ifd, _)) = r.ifd(p)
    {
        tiff.exif = ifd;
    }
    Some(tiff)
}

/// Tag `tag`'s value in `ifd`.
pub(crate) fn get(ifd: &Ifd, tag: u16) -> Option<&Value> {
    ifd.iter().find(|(t, _)| *t == tag).map(|(_, v)| v)
}

impl Tiff {
    /// The JPEG thumbnail an EXIF block's IFD1 carries: its bytes in `data`
    /// (the block the directories were read from).
    pub fn thumbnail<'a>(&self, data: &'a [u8]) -> Option<&'a [u8]> {
        let at = get(&self.ifd1, THUMB_OFFSET)?.number()? as usize;
        let len = get(&self.ifd1, THUMB_LENGTH)?.number()? as usize;
        let bytes = data.get(at..at.checked_add(len)?)?;
        bytes.starts_with(&[0xff, 0xd8]).then_some(bytes)
    }
}

/// The EXIF block of a JPEG: the TIFF structure in its first `Exif\0\0`
/// APP1 segment.
pub(crate) fn jpeg_exif(file: &[u8]) -> Option<&[u8]> {
    jpeg_segments(file).find_map(|(marker, body)| (marker == 0xe1 && body.starts_with(b"Exif\0\0")).then(|| &body[6..]))
}

/// A JPEG's segments before its image data: each marker and its body.
pub(crate) fn jpeg_segments(file: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = if file.starts_with(&[0xff, 0xd8]) { 2 } else { file.len() };
    std::iter::from_fn(move || {
        // Fill bytes (0xff) may pad between segments.
        while file.get(at) == Some(&0xff) && file.get(at + 1) == Some(&0xff) {
            at += 1;
        }
        if file.get(at) != Some(&0xff) {
            return None;
        }
        let marker = *file.get(at + 1)?;
        // Start of scan: the segments are over.
        if marker == 0xda || marker == 0xd9 {
            return None;
        }
        let len = usize::from(u16::from_be_bytes([*file.get(at + 2)?, *file.get(at + 3)?]));
        let body = file.get(at + 4..at + 2 + len.max(2))?;
        at += 2 + len.max(2);
        Some((marker, body))
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A big-endian TIFF structure of `ifds` (each a list of entries of
    /// SHORT, LONG, RATIONAL or ASCII values), IFD0 chaining to IFD1 when
    /// there are two after the EXIF one, and `thumb` at the end with IFD1
    /// pointing at it: what the tests read.
    pub(crate) fn build(ifd0: &[(u16, Val)], exif: &[(u16, Val)], thumb: Option<&[u8]>) -> Vec<u8> {
        #[derive(Clone)]
        enum Slot {
            Plain(Val),
            ExifPointer,
            ThumbOffset,
        }
        let mut ifds: Vec<Vec<(u16, Slot)>> = vec![ifd0.iter().map(|(t, v)| (*t, Slot::Plain(v.clone()))).collect()];
        if !exif.is_empty() {
            ifds[0].push((EXIF_POINTER, Slot::ExifPointer));
            ifds.push(exif.iter().map(|(t, v)| (*t, Slot::Plain(v.clone()))).collect());
        }
        let thumb_ifd = thumb.map(|t| {
            ifds.push(vec![(THUMB_OFFSET, Slot::ThumbOffset), (THUMB_LENGTH, Slot::Plain(Val::Long(t.len() as u32)))]);
            ifds.len() - 1
        });
        let size = |v: &Val| match v {
            Val::Short(_) => 2,
            Val::Long(_) => 4,
            Val::Rational(..) => 8,
            Val::Ascii(s) => s.len() + 1,
        };
        let mut offsets = Vec::new();
        let mut at = 8;
        for ifd in &ifds {
            offsets.push(at);
            at += 2 + 12 * ifd.len() + 4;
            for (_, s) in ifd {
                if let Slot::Plain(v) = s
                    && size(v) > 4
                {
                    at += size(v);
                }
            }
        }
        let thumb_at = at;
        let mut out = b"MM\0*\0\0\0\x08".to_vec();
        for (i, ifd) in ifds.iter().enumerate() {
            let mut data = Vec::new();
            let mut data_at = offsets[i] + 2 + 12 * ifd.len() + 4;
            out.extend_from_slice(&(ifd.len() as u16).to_be_bytes());
            for (tag, slot) in ifd {
                out.extend_from_slice(&tag.to_be_bytes());
                let (kind, count, mut bytes) = match slot {
                    Slot::Plain(Val::Short(v)) => (3u16, 1u32, v.to_be_bytes().to_vec()),
                    Slot::Plain(Val::Long(v)) => (4, 1, v.to_be_bytes().to_vec()),
                    Slot::Plain(Val::Rational(a, b)) => (5, 1, [a.to_be_bytes(), b.to_be_bytes()].concat()),
                    Slot::Plain(Val::Ascii(s)) => (2, s.len() as u32 + 1, [s.as_bytes(), &[0]].concat()),
                    Slot::ExifPointer => (4, 1, (offsets[1] as u32).to_be_bytes().to_vec()),
                    Slot::ThumbOffset => (4, 1, (thumb_at as u32).to_be_bytes().to_vec()),
                };
                out.extend_from_slice(&kind.to_be_bytes());
                out.extend_from_slice(&count.to_be_bytes());
                if bytes.len() <= 4 {
                    bytes.resize(4, 0);
                    out.extend_from_slice(&bytes);
                } else {
                    out.extend_from_slice(&(data_at as u32).to_be_bytes());
                    data_at += bytes.len();
                    data.extend_from_slice(&bytes);
                }
            }
            let next = if i == 0 { thumb_ifd.map_or(0, |t| offsets[t] as u32) } else { 0 };
            out.extend_from_slice(&next.to_be_bytes());
            out.extend_from_slice(&data);
        }
        if let Some(t) = thumb {
            out.extend_from_slice(t);
        }
        out
    }

    #[derive(Clone, Debug)]
    pub(crate) enum Val {
        Short(u16),
        Long(u32),
        Rational(u32, u32),
        Ascii(&'static str),
    }

    #[test]
    fn directories_and_values_are_read() {
        let tiff = build(
            &[(0x0112, Val::Short(6)), (0x010f, Val::Ascii("Maker")), (0x011a, Val::Rational(300, 1))],
            &[(0x829a, Val::Rational(1, 100)), (0xa002, Val::Long(640))],
            Some(&[0xff, 0xd8, 0xff, 0xd9]),
        );
        let t = parse(&tiff).expect("a TIFF structure");
        assert_eq!(get(&t.ifd0, 0x0112), Some(&Value::Ints(vec![6])));
        assert_eq!(get(&t.ifd0, 0x010f), Some(&Value::Ascii("Maker".into())));
        assert_eq!(get(&t.ifd0, 0x011a), Some(&Value::Reals(vec![300.0])));
        assert_eq!(get(&t.exif, 0x829a), Some(&Value::Reals(vec![0.01])));
        assert_eq!(get(&t.exif, 0xa002), Some(&Value::Ints(vec![640])));
        assert_eq!(t.thumbnail(&tiff), Some(&[0xff, 0xd8, 0xff, 0xd9][..]));
        assert_eq!(t.pages, 2);
    }

    #[test]
    fn malformed_structures_read_as_less() {
        assert!(parse(b"not a tiff").is_none());
        assert!(parse(b"MM\0*").is_none());
        // A directory pointing at itself is read once.
        let mut looped = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0".to_vec();
        looped.extend_from_slice(&8u32.to_be_bytes());
        let t = parse(&looped).expect("one directory");
        assert_eq!(t.pages, 1);
        assert_eq!(get(&t.ifd0, 0x0112), Some(&Value::Ints(vec![6])));
        // An entry whose value lies past the end is left out.
        let mut past = b"MM\0*\0\0\0\x08\0\x01\x01\x0f\0\x02\0\0\0\x40".to_vec();
        past.extend_from_slice(&0x1000u32.to_be_bytes());
        past.extend_from_slice(&0u32.to_be_bytes());
        assert!(parse(&past).expect("a directory").ifd0.is_empty());
        // Truncated anywhere, it never panics.
        let whole = build(
            &[(0x0112, Val::Short(3)), (0x010f, Val::Ascii("Some maker"))],
            &[(0x829a, Val::Rational(1, 8))],
            None,
        );
        for n in 0..whole.len() {
            let _ = parse(&whole[..n]);
        }
    }

    #[test]
    fn jpeg_segments_are_walked_to_the_scan() {
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, b'J', b'F'];
        jpeg.extend_from_slice(&[0xff, 0xe1, 0, 10]);
        jpeg.extend_from_slice(b"Exif\0\0MM");
        jpeg.extend_from_slice(&[0xff, 0xda, 0, 2, 0xff, 0xe1, 0, 4, 0, 0]);
        let segments: Vec<u8> = jpeg_segments(&jpeg).map(|(m, _)| m).collect();
        assert_eq!(segments, [0xe0, 0xe1]);
        assert_eq!(jpeg_exif(&jpeg), Some(&b"MM"[..]));
        assert_eq!(jpeg_segments(b"\xff\xd8\xff\xe0\x00").count(), 0);
    }
}
