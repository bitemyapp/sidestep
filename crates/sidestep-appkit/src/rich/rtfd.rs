//! Flat RTFD (`com.apple.flat-rtfd`): an RTFD package (a directory holding
//! `TXT.rtf` and any attachments' files) serialized as one piece of data,
//! which is what macOS puts on the pasteboard and `RTFDFromRange:` gives.
//!
//! The layout, learned from what AppKit writes on macOS: `rtfd`, four zero
//! bytes, then little-endian 32-bit numbers: 3, the entry count, each
//! entry's name (length and bytes), each entry's length, and the entries:
//! each a 1, a length and its bytes. The directory's own entry, `.`, holds
//! the other entries' names, each with a 16-byte record (modification
//! time, permissions, eight zero bytes), laid out the same way. Only
//! `TXT.rtf` is read and written: attachments come with
//! `NSTextAttachment`.

/// Whether `data` is flat RTFD.
pub(crate) fn is_rtfd(data: &[u8]) -> bool {
    data.starts_with(b"rtfd\0\0\0\0")
}

struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn u32(&mut self) -> Option<usize> {
        let b = self.data.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let b = self.data.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(b)
    }
}

/// The RTF of a flat RTFD: its `TXT.rtf`.
pub(crate) fn read(data: &[u8]) -> Option<&[u8]> {
    if !is_rtfd(data) {
        return None;
    }
    let mut c = Cursor { data, at: 8 };
    let _version = c.u32()?;
    let count = c.u32()?;
    if count > 4096 {
        return None;
    }
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let n = c.u32()?;
        names.push(c.bytes(n)?);
    }
    let mut lengths = Vec::with_capacity(count);
    for _ in 0..count {
        lengths.push(c.u32()?);
    }
    for (name, len) in names.iter().zip(lengths) {
        let entry = c.bytes(len)?;
        if *name == b"TXT.rtf" {
            let mut e = Cursor { data: entry, at: 0 };
            let _kind = e.u32()?;
            let n = e.u32()?;
            return e.bytes(n);
        }
    }
    None
}

/// `rtf` as flat RTFD, modified at `mtime` (seconds since 1970).
pub(crate) fn write(rtf: &[u8], mtime: u32) -> Vec<u8> {
    let u = |out: &mut Vec<u8>, n: usize| out.extend_from_slice(&(n as u32).to_le_bytes());
    // The file's entry, and the directory's (its files' records).
    let mut file = Vec::with_capacity(rtf.len() + 8);
    u(&mut file, 1);
    u(&mut file, rtf.len());
    file.extend_from_slice(rtf);
    let mut records = Vec::new();
    u(&mut records, 1);
    u(&mut records, 7);
    records.extend_from_slice(b"TXT.rtf");
    u(&mut records, 16);
    records.extend_from_slice(&mtime.to_le_bytes());
    records.extend_from_slice(&0o666u32.to_le_bytes());
    records.extend_from_slice(&[0; 8]);
    let mut dir = Vec::with_capacity(records.len() + 8);
    u(&mut dir, 1);
    u(&mut dir, records.len());
    dir.extend_from_slice(&records);
    let mut out = b"rtfd\0\0\0\0".to_vec();
    u(&mut out, 3);
    u(&mut out, 2);
    for name in [&b"TXT.rtf"[..], b"."] {
        u(&mut out, name.len());
        out.extend_from_slice(name);
    }
    u(&mut out, file.len());
    u(&mut out, dir.len());
    out.extend_from_slice(&file);
    out.extend_from_slice(&dir);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What AppKit wrote on macOS for "hi" in Helvetica 12.
    const APPKIT: &[u8] = include_bytes!("testdata/hi.flat-rtfd");

    #[test]
    fn appkit_rtfd_reads() {
        let rtf = read(APPKIT).expect("TXT.rtf");
        assert!(rtf.starts_with(b"{\\rtf1\\ansi\\ansicpg1252\\cocoartf2870") && rtf.ends_with(b"hi}"));
        assert_eq!(read(b"rtfd\0\0\0\0\x03\0\0\0"), None);
        assert_eq!(read(b"{\\rtf1 x}"), None);
    }

    #[test]
    fn written_as_appkit_writes_it() {
        let rtf = read(APPKIT).unwrap();
        // The same bytes but the modification time.
        let mtime = u32::from_le_bytes(APPKIT[APPKIT.len() - 16..APPKIT.len() - 12].try_into().unwrap());
        assert_eq!(write(rtf, mtime), APPKIT);
        assert_eq!(read(&write(b"{\\rtf1 x}", 0)), Some(&b"{\\rtf1 x}"[..]));
    }
}
