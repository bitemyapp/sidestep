//! The image file types ImageIO reads and writes here, by their uniform
//! type identifiers, and which one a file is (by its signature, as ImageIO
//! recognizes files by content rather than by name).

use std::sync::Mutex;

use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSString};
use sidestep_runtime::ObjectRef;

/// A file type the codecs (`crate::codec`) read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Png,
    Jpeg,
    Gif,
    WebP,
    Bmp,
    Tiff,
    Ico,
}

// The identifiers, as constant strings (CGImageSourceGetType hands them out
// without a reference of the caller's own, as ImageIO does).
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepImageIOTypePNG = "public.png"
);
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepImageIOTypeJPEG = "public.jpeg"
);
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepImageIOTypeGIF = "com.compuserve.gif"
);
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepImageIOTypeWebP = "org.webmproject.webp"
);
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepImageIOTypeBMP = "com.microsoft.bmp"
);
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepImageIOTypeTIFF = "public.tiff"
);
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepImageIOTypeICO = "com.microsoft.ico"
);

/// What sources read, in the order `CGImageSourceCopyTypeIdentifiers`
/// lists them (macOS's order, with the types the codecs lack left out).
pub(crate) const READABLE: [Kind; 7] = [Kind::Jpeg, Kind::Png, Kind::Gif, Kind::Tiff, Kind::Ico, Kind::Bmp, Kind::WebP];

/// What destinations write, in the order
/// `CGImageDestinationCopyTypeIdentifiers` lists them.
pub(crate) const WRITABLE: [Kind; 5] = [Kind::Jpeg, Kind::Png, Kind::Gif, Kind::Tiff, Kind::Bmp];

impl Kind {
    pub fn uti(self) -> &'static str {
        match self {
            Kind::Png => "public.png",
            Kind::Jpeg => "public.jpeg",
            Kind::Gif => "com.compuserve.gif",
            Kind::WebP => "org.webmproject.webp",
            Kind::Bmp => "com.microsoft.bmp",
            Kind::Tiff => "public.tiff",
            Kind::Ico => "com.microsoft.ico",
        }
    }

    /// The identifier as a constant string.
    pub fn constant(self) -> &'static ObjectRef {
        match self {
            Kind::Png => &_SidestepImageIOTypePNG,
            Kind::Jpeg => &_SidestepImageIOTypeJPEG,
            Kind::Gif => &_SidestepImageIOTypeGIF,
            Kind::WebP => &_SidestepImageIOTypeWebP,
            Kind::Bmp => &_SidestepImageIOTypeBMP,
            Kind::Tiff => &_SidestepImageIOTypeTIFF,
            Kind::Ico => &_SidestepImageIOTypeICO,
        }
    }

    /// The type an identifier names, if the codecs read it.
    pub fn of_uti(uti: &str) -> Option<Kind> {
        READABLE.into_iter().find(|k| k.uti() == uti)
    }

    /// The file name extension a file of this type takes.
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Jpeg => "jpeg",
            Kind::Gif => "gif",
            Kind::WebP => "webp",
            Kind::Bmp => "bmp",
            Kind::Tiff => "tiff",
            Kind::Ico => "ico",
        }
    }

    /// The type a file name extension names, whatever its case.
    pub fn of_extension(ext: &str) -> Option<Kind> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "png" => Kind::Png,
            "jpg" | "jpeg" | "jpe" => Kind::Jpeg,
            "gif" => Kind::Gif,
            "webp" => Kind::WebP,
            "bmp" => Kind::Bmp,
            "tif" | "tiff" => Kind::Tiff,
            "ico" => Kind::Ico,
            _ => return None,
        })
    }

    /// The type of a file, by its first bytes: as many as a type's
    /// signature needs, so a file that has only begun to arrive is known as
    /// soon as they're there.
    pub fn sniff(bytes: &[u8]) -> Option<Kind> {
        let kind = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Kind::Png
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Kind::Jpeg
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Kind::Gif
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Kind::WebP
        } else if bytes.starts_with(b"BM") && bytes.len() >= 14 {
            Kind::Bmp
        } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
            Kind::Tiff
        } else if bytes.starts_with(&[0, 0, 1, 0]) && bytes.len() >= 6 && bytes[4..6] != [0, 0] {
            Kind::Ico
        } else {
            return None;
        };
        allowed(kind).then_some(kind)
    }
}

/// The types `CGImageSourceSetAllowableTypes` restricts reading to, if it
/// was called.
static ALLOWED: Mutex<Option<Vec<Kind>>> = Mutex::new(None);

fn allowed(kind: Kind) -> bool {
    ALLOWED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_none_or(|a| a.contains(&kind))
}

/// Read only `kinds` from now on (the process's sources; files of other
/// types read as unknown ones).
pub(crate) fn set_allowed(kinds: Vec<Kind>) {
    *ALLOWED.lock().unwrap_or_else(|e| e.into_inner()) = Some(kinds);
}

/// `kinds`' identifiers, as an array of strings.
pub(crate) fn identifiers(kinds: &[Kind]) -> Retained<NSArray<NSString>> {
    let strings: Vec<Retained<NSString>> = kinds.iter().map(|k| NSString::from_str(k.uti())).collect();
    NSArray::from_retained_slice(&strings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_known_by_their_signatures() {
        assert_eq!(Kind::sniff(b"\x89PNG\r\n\x1a\n"), Some(Kind::Png));
        assert_eq!(Kind::sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some(Kind::Jpeg));
        assert_eq!(Kind::sniff(b"GIF89a"), Some(Kind::Gif));
        assert_eq!(Kind::sniff(b"RIFF\0\0\0\0WEBPVP8L"), Some(Kind::WebP));
        assert_eq!(Kind::sniff(b"MM\0*\0\0\0\x08"), Some(Kind::Tiff));
        assert_eq!(Kind::sniff(b"BM\0\0\0\0\0\0\0\0\0\0\0\0"), Some(Kind::Bmp));
        assert_eq!(Kind::sniff(&[0, 0, 1, 0, 1, 0]), Some(Kind::Ico));
        assert_eq!(Kind::sniff(b"hello, world"), None);
        assert_eq!(Kind::sniff(b"\x89PN"), None);
        // HEIC isn't read.
        assert_eq!(Kind::sniff(b"\0\0\0\x18ftypheic\0\0\0\0"), None);
        assert_eq!(Kind::of_extension("JPG"), Some(Kind::Jpeg));
        assert_eq!(Kind::of_uti("public.heic"), None);
    }
}
