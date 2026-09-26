//! Pasteboard types: their old names, which types conform to which, and
//! how they travel between programs on Linux.
//!
//! A pasteboard asked for types (`availableTypeFromArray:`, and a drag's
//! destination for the types it registered) finds, for each type asked for
//! in turn, that type if it's there, else the first type there that is one
//! of its kinds (`public.png` for `public.image`, `public.file-url` for
//! `public.url`), as macOS does.
//!
//! AppKit names types with uniform type identifiers (`public.png`), and
//! still takes the names they had before those (`NSStringPboardType`,
//! `NeXT TIFF v4.0 pasteboard type`): each old name reads and writes the
//! type it became, and a pasteboard lists it right after that type, as
//! macOS does. Two old names are different: `NSFilenamesPboardType` and
//! `Apple URL pasteboard type` hold property lists (the paths of the file
//! URLs, and a URL with an empty title) that a pasteboard makes from its
//! URLs when asked, and writing them doesn't write URLs.
//!
//! Wayland names types with MIME types, so the general pasteboard and
//! drags map between the two (`mimes_for`, `type_for_mime`):
//!
//! | type | MIME types |
//! |---|---|
//! | `public.utf8-plain-text` | `text/plain;charset=utf-8`, `UTF8_STRING`, `text/plain`, `STRING`, `TEXT` |
//! | `public.html` | `text/html` |
//! | `public.rtf` | `text/rtf`, `application/rtf` |
//! | `public.png`, `public.tiff`, `public.jpeg` | `image/png`, `image/tiff`, `image/jpeg` |
//! | `com.adobe.pdf` | `application/pdf` |
//! | `public.utf8-tab-separated-values-text` | `text/tab-separated-values` |
//! | `public.file-url`, `public.url` | `text/uri-list` (one URI per item), and to read, `x-special/gnome-copied-files` and `text/x-moz-url` |
//! | a MIME type (it has a `/`) | itself |
//! | any other type | `application/x-sidestep-uti.<type>` |
//!
//! so Sidestep programs exchange any type among themselves, and other
//! programs see the ones they know.

use std::borrow::Cow;

use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSString};

/// The types' identifiers, as AppKit's constants hold them.
pub(crate) const STRING: &str = "public.utf8-plain-text";
pub(crate) const FILE_URL: &str = "public.file-url";
pub(crate) const URL: &str = "public.url";
pub(crate) const HTML: &str = "public.html";

/// The two old names whose values a pasteboard makes from its URLs.
pub(crate) const FILENAMES: &str = "NSFilenamesPboardType";
pub(crate) const OLD_URL: &str = "Apple URL pasteboard type";

/// Types and their old names, which read and write them.
const OLD_NAMES: &[(&str, &str)] = &[
    (STRING, "NSStringPboardType"),
    ("public.tiff", "NeXT TIFF v4.0 pasteboard type"),
    ("public.png", "Apple PNG pasteboard type"),
    ("public.rtf", "NeXT Rich Text Format v1.0 pasteboard type"),
    ("com.apple.flat-rtfd", "NeXT RTFD pasteboard type"),
    ("public.utf8-tab-separated-values-text", "NeXT tabular text pasteboard type"),
    ("com.apple.cocoa.pasteboard.character-formatting", "NeXT font pasteboard type"),
    ("com.apple.cocoa.pasteboard.paragraph-formatting", "NeXT ruler pasteboard type"),
    ("com.apple.cocoa.pasteboard.color", "NSColor pasteboard type"),
    (HTML, "Apple HTML pasteboard type"),
    ("com.adobe.pdf", "Apple PDF pasteboard type"),
    ("com.apple.cocoa.pasteboard.multiple-text-selection", "Apple multiple text selection pasteboard type"),
    ("com.adobe.encapsulated-postscript", "NeXT Encapsulated PostScript v1.2 pasteboard type"),
    ("public.vcard", "Apple VCard pasteboard type"),
    ("com.apple.ink.inktext", "Apple InkText pasteboard type"),
    ("com.apple.cocoa.pasteboard.sound", "NSSoundPboardType"),
    ("com.apple.cocoa.pasteboard.find-panel-search-options", "NSFindPanel search options pasteboard type"),
];

/// A type as it's stored: an old name as the type it names now.
pub(crate) fn canonical(kind: &str) -> &str {
    OLD_NAMES.iter().find(|(_, old)| *old == kind).map_or(kind, |(new, _)| new)
}

/// The old name a pasteboard lists after `kind`, if it has one.
pub(crate) fn old_name(kind: &str) -> Option<&'static str> {
    match kind {
        FILE_URL => Some(FILENAMES),
        URL => Some(OLD_URL),
        _ => OLD_NAMES.iter().find(|(new, _)| *new == kind).map(|(_, old)| *old),
    }
}

/// The type an `NSString` names, as it's stored.
pub(crate) fn from_ns(kind: &NSString) -> String {
    let kind = kind.to_string();
    match canonical(&kind) {
        same if same.len() == kind.len() => kind,
        new => new.to_owned(),
    }
}

/// Types each type conforms to directly, as the system's type tree has
/// them, for `canReadItemWithDataConformingToTypes:`. Every type conforms
/// to `public.data` and `public.item`.
const PARENTS: &[(&str, &[&str])] = &[
    (STRING, &["public.plain-text"]),
    ("public.utf16-plain-text", &["public.plain-text"]),
    ("public.utf16-external-plain-text", &["public.plain-text"]),
    ("public.plain-text", &["public.text"]),
    ("public.utf8-tab-separated-values-text", &["public.tab-separated-values-text", STRING]),
    ("public.tab-separated-values-text", &["public.delimited-values-text"]),
    ("public.delimited-values-text", &["public.text"]),
    (HTML, &["public.text"]),
    ("public.rtf", &["public.text"]),
    ("public.text", &["public.content"]),
    ("com.apple.flat-rtfd", &["public.composite-content"]),
    ("com.adobe.pdf", &["public.composite-content"]),
    ("public.composite-content", &["public.content"]),
    ("public.png", &["public.image"]),
    ("public.tiff", &["public.image"]),
    ("public.jpeg", &["public.image"]),
    ("public.image", &["public.content"]),
    (FILE_URL, &[URL]),
    ("public.vcard", &["public.contact"]),
];

/// Whether `kind` is `target` or one of its kinds.
pub(crate) fn conforms(kind: &str, target: &str) -> bool {
    if kind == target || target == "public.data" || target == "public.item" {
        return true;
    }
    let parents = PARENTS.iter().find(|(k, _)| *k == kind).map_or(&[][..], |(_, p)| p);
    parents.iter().any(|p| conforms(p, target))
}

/// How `wanted` (a type as stored, see `from_ns`) is found among `kinds`,
/// a pasteboard's types in the order written.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found<'a> {
    /// It's there itself (for the two old URL names, a URL they're made of).
    Itself,
    /// This type, which is one of its kinds, is.
    Kind(&'a str),
}

pub(crate) fn find<'a>(kinds: &'a [String], wanted: &str) -> Option<Found<'a>> {
    let has = |kind: &str| kinds.iter().any(|k| k == kind);
    let itself = match wanted {
        FILENAMES => has(FILE_URL),
        OLD_URL => has(FILE_URL) || has(URL),
        kind => has(kind),
    };
    if itself {
        return Some(Found::Itself);
    }
    kinds.iter().find(|k| conforms(k, wanted)).map(|k| Found::Kind(k))
}

/// `availableTypeFromArray:` for `kinds`: the first of `wanted` found, as
/// it was asked for, or the type found for it.
pub(crate) fn first_available(kinds: &[String], wanted: &NSArray<NSString>) -> Option<Retained<NSString>> {
    wanted.iter().find_map(|w| match find(kinds, &from_ns(&w))? {
        Found::Itself => Some(w),
        Found::Kind(kind) => Some(NSString::from_str(kind)),
    })
}

/// The type a URL is kept as: a file URL, or another.
pub(crate) fn url_kind(uri: &str) -> &'static str {
    if is_file_uri(uri) { FILE_URL } else { URL }
}

/// The MIME types that mark this process's own selections and drags.
pub(crate) const OWNER_PREFIX: &str = "application/x-sidestep-owner";
/// Types with no MIME type of their own travel under this prefix.
const UTI_PREFIX: &str = "application/x-sidestep-uti.";
pub(crate) const URI_LIST: &str = "text/uri-list";
const GNOME_FILES: &str = "x-special/gnome-copied-files";
const MOZ_URL: &str = "text/x-moz-url";

/// The MIME types text is offered and read as, best first.
pub(crate) const TEXT_MIMES: &[&str] = &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"];

/// The MIME types a type is offered as, best first. URLs aren't here:
/// every item's URL goes into one `text/uri-list` (see `uri_list`).
pub(crate) fn mimes_for(kind: &str) -> Vec<Cow<'static, str>> {
    let known: &[&'static str] = match kind {
        STRING => TEXT_MIMES,
        HTML => &["text/html"],
        "public.rtf" => &["text/rtf", "application/rtf"],
        "public.png" => &["image/png"],
        "public.tiff" => &["image/tiff"],
        "public.jpeg" => &["image/jpeg"],
        "com.adobe.pdf" => &["application/pdf"],
        "public.utf8-tab-separated-values-text" => &["text/tab-separated-values"],
        FILE_URL | URL => &[],
        other if other.contains('/') => return vec![Cow::Owned(other.to_owned())],
        other => return vec![Cow::Owned(format!("{UTI_PREFIX}{other}"))],
    };
    known.iter().map(|m| Cow::Borrowed(*m)).collect()
}

/// The pasteboard type another program's MIME type reads as; `None` for
/// the ones read otherwise (URL lists) or not at all (our marker).
pub(crate) fn type_for_mime(mime: &str) -> Option<Cow<'static, str>> {
    let kind = match mime {
        m if TEXT_MIMES.contains(&m) || m.starts_with("text/plain;") => STRING,
        "text/html" => HTML,
        "text/rtf" | "application/rtf" => "public.rtf",
        "image/png" => "public.png",
        "image/tiff" => "public.tiff",
        "image/jpeg" => "public.jpeg",
        "application/pdf" => "com.adobe.pdf",
        "text/tab-separated-values" => "public.utf8-tab-separated-values-text",
        URI_LIST | GNOME_FILES | MOZ_URL => return None,
        m if m.starts_with(OWNER_PREFIX) => return None,
        m => {
            return Some(
                m.strip_prefix(UTI_PREFIX).map_or_else(|| Cow::Owned(m.to_owned()), |u| Cow::Owned(u.to_owned())),
            );
        }
    };
    Some(Cow::Borrowed(kind))
}

/// Where another program's URLs are, among the MIME types it offers: the
/// best of them, if any.
pub(crate) fn url_mime(mimes: &[String]) -> Option<&'static str> {
    [URI_LIST, GNOME_FILES, MOZ_URL].into_iter().find(|m| mimes.iter().any(|o| o == m))
}

/// The URLs in data of `mime` (see `url_mime`).
pub(crate) fn parse_urls(mime: &str, data: &[u8]) -> Vec<String> {
    match mime {
        URI_LIST => parse_uri_list(&String::from_utf8_lossy(data)),
        GNOME_FILES => {
            // "copy" or "cut", then a URI per line.
            let text = String::from_utf8_lossy(data);
            text.lines().skip(1).map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect()
        }
        MOZ_URL => {
            // UTF-16 (as Firefox writes it) or UTF-8: the URL, then its title.
            let text = decode_moz(data);
            text.lines().next().map(str::trim).filter(|l| !l.is_empty()).map(|l| vec![l.to_owned()]).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// A `text/uri-list`: one URI a line, lines ending in CRLF (or LF), `#`
/// starting a comment line.
pub(crate) fn parse_uri_list(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_owned).collect()
}

/// URIs as a `text/uri-list`.
pub(crate) fn uri_list<'a>(uris: impl IntoIterator<Item = &'a str>) -> String {
    uris.into_iter().fold(String::new(), |mut list, uri| {
        list.push_str(uri);
        list.push_str("\r\n");
        list
    })
}

/// File URIs as file managers copy them (`x-special/gnome-copied-files`).
pub(crate) fn gnome_copied_files<'a>(uris: impl IntoIterator<Item = &'a str>) -> String {
    uris.into_iter().fold(String::from("copy"), |mut list, uri| {
        list.push('\n');
        list.push_str(uri);
        list
    })
}

pub(crate) const GNOME_FILES_MIME: &str = GNOME_FILES;

fn decode_moz(data: &[u8]) -> String {
    let utf16 = data.len() >= 2 && data.len().is_multiple_of(2) && (data.starts_with(&[0xff, 0xfe]) || data[1] == 0);
    if !utf16 {
        return String::from_utf8_lossy(data).into_owned();
    }
    let units = data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c));
    char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).filter(|c| *c != '\u{feff}').collect()
}

/// Whether a URI names a file, to read as `public.file-url` rather than
/// `public.url`.
pub(crate) fn is_file_uri(uri: &str) -> bool {
    uri.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("file:"))
}

/// The path a `file:` URL names, with its escapes decoded, for
/// `NSFilenamesPboardType`.
pub(crate) fn file_path(uri: &str) -> Option<String> {
    let rest = uri.get(5..).filter(|_| is_file_uri(uri))?;
    // file:///path, or file://localhost/path.
    let path = match rest.strip_prefix("//") {
        Some(authority_and_path) => {
            let slash = authority_and_path.find('/')?;
            let host = &authority_and_path[..slash];
            if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
                return None;
            }
            &authority_and_path[slash..]
        }
        None => rest,
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    percent_decode(path)
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A path as a `file:` URL, escaping what a URL can't hold.
pub(crate) fn file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for &b in path.as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

// Constants platform didn't export (lib.rs has the rest), with the values
// macOS gives them (conformance/tests/pasteboard.rs compares).
sidestep_foundation::constant_string!(NSPasteboardNameFont = "Apple CFPasteboard font");
sidestep_foundation::constant_string!(NSPasteboardNameRuler = "Apple CFPasteboard ruler");
sidestep_foundation::constant_string!(NSPasteboardNameDrag = "Apple CFPasteboard drag");
sidestep_foundation::constant_string!(NSPasteboardURLReadingFileURLsOnlyKey = "NSPasteboardURLReadingFileURLsOnlyKey");
sidestep_foundation::constant_string!(
    NSPasteboardURLReadingContentsConformToTypesKey = "NSPasteboardURLReadingContentsConformToTypesKey"
);
sidestep_foundation::constant_string!(
    NSPasteboardTypeFindPanelSearchOptions = "com.apple.cocoa.pasteboard.find-panel-search-options"
);
sidestep_foundation::constant_string!(
    NSPasteboardMetadataTypeContentType = "com.apple.appkit.pasteboard-metadata-type.contenttype"
);
// The old names of the pasteboards and of two types without new ones.
sidestep_foundation::constant_string!(NSGeneralPboard = "Apple CFPasteboard general");
sidestep_foundation::constant_string!(NSFontPboard = "Apple CFPasteboard font");
sidestep_foundation::constant_string!(NSRulerPboard = "Apple CFPasteboard ruler");
sidestep_foundation::constant_string!(NSFindPboard = "Apple CFPasteboard find");
sidestep_foundation::constant_string!(NSDragPboard = "Apple CFPasteboard drag");
sidestep_foundation::constant_string!(NSFileContentsPboardType = "NXFileContentsPboardType");
sidestep_foundation::constant_string!(NSPICTPboardType = "Apple PICT pasteboard type");
// What `detectPatterns…` looks for: named here so programs link, though
// Sidestep detects nothing yet.
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternProbableWebURL = "com.apple.appkit.pasteboard-detection-pattern.probable-web-url"
);
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternProbableWebSearch = "com.apple.appkit.pasteboard-detection-pattern.probable-web-search"
);
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternNumber = "com.apple.appkit.pasteboard-detection-pattern.number"
);
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternLink = "com.apple.appkit.pasteboard-detection-pattern.dd.link"
);
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternPhoneNumber = "com.apple.appkit.pasteboard-detection-pattern.dd.phone"
);
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternEmailAddress = "com.apple.appkit.pasteboard-detection-pattern.dd.email"
);
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternPostalAddress = "com.apple.appkit.pasteboard-detection-pattern.dd.address"
);
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternCalendarEvent = "com.apple.appkit.pasteboard-detection-pattern.dd.event"
);
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternShipmentTrackingNumber = "com.apple.appkit.pasteboard-detection-pattern.dd.shipment"
);
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternFlightNumber = "com.apple.appkit.pasteboard-detection-pattern.dd.flight"
);
sidestep_foundation::constant_string!(
    NSPasteboardDetectionPatternMoneyAmount = "com.apple.appkit.pasteboard-detection-pattern.dd.money"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_names_name_the_new_types() {
        assert_eq!(canonical("NSStringPboardType"), STRING);
        assert_eq!(canonical("Apple PNG pasteboard type"), "public.png");
        assert_eq!(canonical("public.png"), "public.png");
        // URLs' old names hold property lists, not URLs.
        assert_eq!(canonical(OLD_URL), OLD_URL);
        assert_eq!(canonical(FILENAMES), FILENAMES);
        assert_eq!(old_name(HTML), Some("Apple HTML pasteboard type"));
        assert_eq!(old_name(FILE_URL), Some(FILENAMES));
        assert_eq!(old_name("com.example.x"), None);
    }

    #[test]
    fn finding_types() {
        let kinds = |k: &[&str]| k.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        let board = kinds(&[HTML, STRING]);
        assert_eq!(find(&board, STRING), Some(Found::Itself));
        assert_eq!(find(&board, "public.text"), Some(Found::Kind(HTML)));
        assert_eq!(find(&board, "public.plain-text"), Some(Found::Kind(STRING)));
        assert_eq!(find(&board, "public.image"), None);
        let files = kinds(&[FILE_URL]);
        assert_eq!(find(&files, URL), Some(Found::Kind(FILE_URL)));
        assert_eq!(find(&files, FILENAMES), Some(Found::Itself));
        assert_eq!(find(&files, OLD_URL), Some(Found::Itself));
        let links = kinds(&[URL]);
        assert_eq!(find(&links, FILE_URL), None);
        assert_eq!(find(&links, FILENAMES), None);
        assert_eq!(find(&links, OLD_URL), Some(Found::Itself));
        assert_eq!(url_kind("file:///tmp/a"), FILE_URL);
        assert_eq!(url_kind("https://example.com/"), URL);
    }

    #[test]
    fn conformance() {
        assert!(conforms(STRING, "public.text"));
        assert!(conforms(HTML, "public.text"));
        assert!(conforms("public.png", "public.image"));
        assert!(conforms(FILE_URL, URL));
        assert!(!conforms(URL, FILE_URL));
        assert!(!conforms(STRING, FILE_URL));
        assert!(conforms("com.example.x", "public.data"));
        assert!(conforms("public.utf8-tab-separated-values-text", "public.plain-text"));
    }

    #[test]
    fn types_travel_as_mime_types() {
        assert_eq!(mimes_for(STRING)[0], "text/plain;charset=utf-8");
        assert_eq!(mimes_for("image/webp"), ["image/webp"]);
        assert_eq!(mimes_for("com.example.x"), ["application/x-sidestep-uti.com.example.x"]);
        assert!(mimes_for(FILE_URL).is_empty());
        assert_eq!(type_for_mime("UTF8_STRING").as_deref(), Some(STRING));
        assert_eq!(type_for_mime("text/plain;charset=UTF-8").as_deref(), Some(STRING));
        assert_eq!(type_for_mime("application/x-sidestep-uti.com.example.x").as_deref(), Some("com.example.x"));
        assert_eq!(type_for_mime("image/webp").as_deref(), Some("image/webp"));
        assert_eq!(type_for_mime(URI_LIST), None);
        assert_eq!(type_for_mime("application/x-sidestep-owner;pid=1"), None);
        // Every type we offer comes back as itself.
        for kind in [STRING, HTML, "public.rtf", "public.png", "com.adobe.pdf", "com.example.x", "image/webp"] {
            for mime in mimes_for(kind) {
                assert_eq!(type_for_mime(&mime).as_deref(), Some(kind), "{mime}");
            }
        }
    }

    #[test]
    fn url_lists() {
        let list = "# a comment\r\nfile:///tmp/a%20b\r\nhttps://example.com/\r\n\r\n";
        assert_eq!(parse_uri_list(list), ["file:///tmp/a%20b", "https://example.com/"]);
        assert_eq!(uri_list(["file:///a", "file:///b"]), "file:///a\r\nfile:///b\r\n");
        assert_eq!(parse_urls(GNOME_FILES, b"copy\nfile:///a\nfile:///b"), ["file:///a", "file:///b"]);
        assert_eq!(gnome_copied_files(["file:///a"]), "copy\nfile:///a");
        let moz: Vec<u8> = "https://x/\ntitle".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(parse_urls(MOZ_URL, &moz), ["https://x/"]);
        assert_eq!(parse_urls(MOZ_URL, b"https://y/\ntitle"), ["https://y/"]);
        assert_eq!(url_mime(&["text/plain".into(), GNOME_FILES.into(), URI_LIST.into()]), Some(URI_LIST));
    }

    #[test]
    fn file_paths() {
        assert_eq!(file_path("file:///tmp/a%20b").as_deref(), Some("/tmp/a b"));
        assert_eq!(file_path("file://localhost/etc/hosts").as_deref(), Some("/etc/hosts"));
        assert_eq!(file_path("file://elsewhere/x"), None);
        assert_eq!(file_path("https://x/"), None);
        assert_eq!(file_url("/tmp/a b/ü"), "file:///tmp/a%20b/%C3%BC");
        assert_eq!(file_path(&file_url("/tmp/a b/ü")).as_deref(), Some("/tmp/a b/ü"));
    }
}
