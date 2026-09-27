//! Flat RTFD (`com.apple.flat-rtfd`): an RTFD package (a directory holding
//! `TXT.rtf` and its attachments' files) serialized as one piece of data,
//! which is what macOS puts on the pasteboard and `RTFDFromRange:` gives.
//!
//! The layout, learned from what AppKit writes on macOS: `rtfd`, four zero
//! bytes, then little-endian 32-bit numbers: 3, the entry count, each
//! entry's name (length and bytes), each entry's length, and the entries:
//! each a 1, a length and its bytes (or, for a large file AppKit aligns to
//! a page, a length with its top bit set, then the file's length and the
//! padding's, the padding and the file). The directory's own entry, `.`,
//! holds the other entries' names, each with a 16-byte record
//! (modification time, permissions, eight zero bytes), laid out the same
//! way. `TXT.rtf` names its attachments' files (`\NeXTGraphic`).

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

/// A flat RTFD's parts: `TXT.rtf`, and the other files, by name.
pub(crate) struct Package<'a> {
    pub rtf: &'a [u8],
    pub files: Vec<(String, Vec<u8>)>,
}

/// The parts of a flat RTFD, if it is one with a `TXT.rtf`.
pub(crate) fn read(data: &[u8]) -> Option<Package<'_>> {
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
    let (mut rtf, mut files) = (None, Vec::new());
    for (name, len) in names.iter().zip(lengths) {
        let entry = c.bytes(len)?;
        let mut e = Cursor { data: entry, at: 0 };
        let _kind = e.u32()?;
        let n = e.u32()?;
        let contents = if n & 0x8000_0000 != 0 {
            // A page-aligned file: its length, the padding's, the padding.
            let (n, pad) = (e.u32()?, e.u32()?);
            e.bytes(pad)?;
            e.bytes(n)?
        } else {
            e.bytes(n)?
        };
        match *name {
            b"TXT.rtf" => rtf = Some(contents),
            b"." => {}
            _ => files.push((String::from_utf8_lossy(name).into_owned(), contents.to_vec())),
        }
    }
    Some(Package { rtf: rtf?, files })
}

/// `rtf` and the files `files` (by name) as flat RTFD, modified at `mtime`
/// (seconds since 1970).
pub(crate) fn write(rtf: &[u8], files: &[(String, Vec<u8>)], mtime: u32) -> Vec<u8> {
    let u = |out: &mut Vec<u8>, n: usize| out.extend_from_slice(&(n as u32).to_le_bytes());
    // The files' entries (the attachments', then the text's), then the
    // directory's (its files' records).
    let mut entries: Vec<(&[u8], Vec<u8>)> = Vec::with_capacity(files.len() + 2);
    for (name, contents) in files.iter().map(|(n, c)| (n.as_bytes(), c.as_slice())).chain([(&b"TXT.rtf"[..], rtf)]) {
        let mut entry = Vec::with_capacity(contents.len() + 8);
        u(&mut entry, 1);
        u(&mut entry, contents.len());
        entry.extend_from_slice(contents);
        entries.push((name, entry));
    }
    let mut records = Vec::new();
    u(&mut records, entries.len());
    for (name, _) in &entries {
        u(&mut records, name.len());
        records.extend_from_slice(name);
    }
    for _ in &entries {
        u(&mut records, 16);
    }
    for _ in &entries {
        records.extend_from_slice(&mtime.to_le_bytes());
        records.extend_from_slice(&0o666u32.to_le_bytes());
        records.extend_from_slice(&[0; 8]);
    }
    let mut dir = Vec::with_capacity(records.len() + 8);
    u(&mut dir, 1);
    u(&mut dir, records.len());
    dir.extend_from_slice(&records);
    entries.push((b".", dir));
    let mut out = b"rtfd\0\0\0\0".to_vec();
    u(&mut out, 3);
    u(&mut out, entries.len());
    for (name, _) in &entries {
        u(&mut out, name.len());
        out.extend_from_slice(name);
    }
    for (_, entry) in &entries {
        u(&mut out, entry.len());
    }
    for (_, entry) in &entries {
        out.extend_from_slice(entry);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What AppKit wrote on macOS for "hi" in Helvetica 12.
    const APPKIT: &[u8] = include_bytes!("testdata/hi.flat-rtfd");

    #[test]
    fn appkit_rtfd_reads() {
        let rtf = read(APPKIT).expect("TXT.rtf").rtf;
        assert!(rtf.starts_with(b"{\\rtf1\\ansi\\ansicpg1252\\cocoartf2870") && rtf.ends_with(b"hi}"));
        assert!(read(b"rtfd\0\0\0\0\x03\0\0\0").is_none());
        assert!(read(b"{\\rtf1 x}").is_none());
    }

    #[test]
    fn written_as_appkit_writes_it() {
        let rtf = read(APPKIT).unwrap().rtf;
        // The same bytes but the modification time.
        let mtime = u32::from_le_bytes(APPKIT[APPKIT.len() - 16..APPKIT.len() - 12].try_into().unwrap());
        assert_eq!(write(rtf, &[], mtime), APPKIT);
        assert_eq!(read(&write(b"{\\rtf1 x}", &[], 0)).map(|p| p.rtf), Some(&b"{\\rtf1 x}"[..]));
    }

    #[test]
    fn attachments_files_travel_with_the_text() {
        let files = vec![("Attachment.png".to_owned(), vec![1, 2, 3]), ("b.txt".to_owned(), b"hi".to_vec())];
        let data = write(b"{\\rtf1 x}", &files, 7);
        let p = read(&data).expect("a package");
        assert_eq!((p.rtf, p.files), (&b"{\\rtf1 x}"[..], files));
    }

    #[test]
    fn page_aligned_files_are_read() {
        // As AppKit writes a large file: the length's top bit set, then the
        // file's length and the padding's, the padding and the file.
        let u = |n: u32| n.to_le_bytes();
        let file = [&u(1)[..], &u(0x8000_0000), &u(3), &u(5), &[0; 5], b"abc"].concat();
        let text = [&u(1)[..], &u(9), b"{\\rtf1 x}"].concat();
        let data = [
            &b"rtfd\0\0\0\0"[..],
            &u(3),
            &u(2),
            &u(5),
            b"a.png",
            &u(7),
            b"TXT.rtf",
            &u(file.len() as u32),
            &u(text.len() as u32),
            &file,
            &text,
        ]
        .concat();
        let p = read(&data).expect("a package");
        assert_eq!(p.files, [("a.png".to_owned(), b"abc".to_vec())]);
        assert_eq!(p.rtf, b"{\\rtf1 x}");
    }
}
