//! Rich text interchange: attributed strings read from and written as RTF,
//! flat RTFD, HTML and plain text, as AppKit's additions to
//! `NSAttributedString` do (`initWithData:options:documentAttributes:error:`,
//! `dataFromRange:documentAttributes:error:`, `RTFFromRange:…` and the
//! rest), and attributed strings on the pasteboard (`NSPasteboardReading`
//! and `NSPasteboardWriting`).
//!
//! The formats are read and written as plain data ([`model::Doc`]) by
//! `rtf_read`, `rtf_write`, `rtfd`, `html_read` and `html_write`, all
//! written for Sidestep (RTF from Microsoft's published specification, the
//! rest from what AppKit reads and writes on macOS); `convert` turns
//! documents into attributed strings and back. The methods are added to
//! Foundation's classes by link-time categories.
//!
//! Reading, as AppKit reads (measured on macOS):
//!
//! - The type comes from `NSDocumentTypeDocumentOption`, else from the
//!   data: flat RTFD, RTF (`{\rtf` at the start), an HTML document
//!   (`<html`, `<!DOCTYPE html` or `<head` at the start), else plain text.
//!   A type Sidestep doesn't read (Word, web archives, …) is
//!   `NSTextReadInapplicableDocumentTypeError` (65806); data that isn't RTF
//!   read as RTF is `NSFileReadUnknownError` (256), RTF cut short
//!   `NSFileReadCorruptFileError` (259).
//! - Plain text is in the encoding the options give, else UTF-8 (after a
//!   byte order mark, UTF-16), else Mac OS Roman, in the options' default
//!   attributes or Helvetica 12. HTML is in the encoding the options, a byte
//!   order mark or a `<meta>` give, else Windows-1252 (from the pasteboard,
//!   UTF-8 where the bytes are UTF-8, as Linux programs put it there).
//! - The document attributes are AppKit's for each format (see
//!   `convert::document_attributes`).
//!
//! Writing takes `NSDocumentTypeDocumentAttribute` (plain text, RTF, RTFD
//! or HTML; anything else, or none, is
//! `NSTextWriteInapplicableDocumentTypeError`, 66062) and the document
//! attributes RTF and HTML have room for. Plain text is UTF-8 unless
//! `NSCharacterEncodingDocumentAttribute` says (UTF-16 with a byte order
//! mark, little-endian).
//!
//! On the pasteboard an attributed string is RTF, HTML and plain text (AppKit
//! writes RTF and plain text; HTML is there for the Linux programs that
//! read no RTF, browsers among them), and reads from flat RTFD, RTF, HTML
//! or plain text, in that order.

mod convert;
mod css;
mod html_lex;
mod html_read;
mod html_write;
mod model;
mod rtf_read;
mod rtf_write;
mod rtfd;
mod tables;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{ClassType, Message, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSAttributedString, NSCocoaErrorDomain, NSData, NSDictionary, NSError, NSMutableAttributedString, NSRange,
    NSString, NSURL,
};

use self::convert::{Format, any};
use self::model::Doc;
use crate::pasteboard_types::{HTML as HTML_TYPE, RTF as RTF_TYPE, RTFD as RTFD_TYPE, STRING as STRING_TYPE};

type Dict = NSDictionary<NSString, AnyObject>;

// Document types, attribute keys and options, with AppKit's values: each
// exported, and those Sidestep reads or writes also in `keys` as strings.
macro_rules! constants {
    ($($name:ident = $value:literal $(=> $key:ident)?;)*) => {
        $(sidestep_foundation::constant_string!($name = $value);)*

        /// The dictionary keys Sidestep reads and writes.
        pub(crate) mod keys {
            $($(pub const $key: &str = $value;)?)*
        }
    };
}

constants! {
    NSPlainTextDocumentType = "NSPlainText";
    NSRTFTextDocumentType = "NSRTF";
    NSRTFDTextDocumentType = "NSRTFD";
    NSHTMLTextDocumentType = "NSHTML";
    NSMacSimpleTextDocumentType = "NSMacSimpleText";
    NSDocFormatTextDocumentType = "NSDocFormat";
    NSWordMLTextDocumentType = "NSWordML";
    NSWebArchiveTextDocumentType = "NSWebArchive";
    NSOfficeOpenXMLTextDocumentType = "NSOfficeOpenXML";
    NSOpenDocumentTextDocumentType = "NSOpenDocument";
    NSDocumentTypeDocumentAttribute = "DocumentType" => DOCUMENT_TYPE;
    NSCharacterEncodingDocumentAttribute = "CharacterEncoding" => CHARACTER_ENCODING;
    NSDefaultAttributesDocumentAttribute = "DefaultAttributes" => DEFAULT_ATTRIBUTES;
    NSPaperSizeDocumentAttribute = "PaperSize" => PAPER_SIZE;
    NSLeftMarginDocumentAttribute = "LeftMargin" => LEFT_MARGIN;
    NSRightMarginDocumentAttribute = "RightMargin" => RIGHT_MARGIN;
    NSTopMarginDocumentAttribute = "TopMargin" => TOP_MARGIN;
    NSBottomMarginDocumentAttribute = "BottomMargin" => BOTTOM_MARGIN;
    NSViewSizeDocumentAttribute = "ViewSize" => VIEW_SIZE;
    NSViewZoomDocumentAttribute = "ViewZoom" => VIEW_ZOOM;
    NSViewModeDocumentAttribute = "ViewMode" => VIEW_MODE;
    NSDefaultFontExcludedDocumentAttribute = "NoDefaultFonts";
    NSReadOnlyDocumentAttribute = "ReadOnly" => READ_ONLY;
    NSBackgroundColorDocumentAttribute = "BackgroundColor" => BACKGROUND_COLOR;
    NSHyphenationFactorDocumentAttribute = "HyphenationFactor" => HYPHENATION_FACTOR;
    NSDefaultTabIntervalDocumentAttribute = "DefaultTabInterval" => DEFAULT_TAB_INTERVAL;
    NSTextLayoutSectionsAttribute = "NSTextLayoutSectionsAttribute";
    NSTextLayoutSectionOrientation = "NSTextLayoutSectionOrientation";
    NSTextLayoutSectionRange = "NSTextLayoutSectionRange";
    NSTextScalingDocumentAttribute = "TextScaling" => TEXT_SCALING;
    NSSourceTextScalingDocumentAttribute = "SourceTextScaling";
    NSCocoaVersionDocumentAttribute = "CocoaRTFVersion" => COCOA_VERSION;
    NSConvertedDocumentAttribute = "Converted";
    NSFileTypeDocumentAttribute = "UTI" => FILE_TYPE;
    NSTitleDocumentAttribute = "NSTitleDocumentAttribute" => TITLE;
    NSCompanyDocumentAttribute = "NSCompanyDocumentAttribute" => COMPANY;
    NSCopyrightDocumentAttribute = "NSCopyrightDocumentAttribute" => COPYRIGHT;
    NSSubjectDocumentAttribute = "NSSubjectDocumentAttribute" => SUBJECT;
    NSAuthorDocumentAttribute = "NSAuthorDocumentAttribute" => AUTHOR;
    NSKeywordsDocumentAttribute = "NSKeywordsDocumentAttribute" => KEYWORDS;
    NSCommentDocumentAttribute = "NSCommentDocumentAttribute" => COMMENT;
    NSEditorDocumentAttribute = "NSEditorDocumentAttribute" => EDITOR;
    NSCreationTimeDocumentAttribute = "NSCreationTimeDocumentAttribute";
    NSModificationTimeDocumentAttribute = "NSModificationTimeDocumentAttribute";
    NSManagerDocumentAttribute = "NSManagerDocumentAttribute" => MANAGER;
    NSCategoryDocumentAttribute = "NSCategoryDocumentAttribute" => CATEGORY;
    NSAppearanceDocumentAttribute = "NSAppearanceDocumentAttribute";
    NSExcludedElementsDocumentAttribute = "ExcludedElements";
    NSTextEncodingNameDocumentAttribute = "TextEncodingName" => TEXT_ENCODING_NAME;
    NSPrefixSpacesDocumentAttribute = "PrefixSpaces";
    NSUsesScreenFontsDocumentAttribute = "UsesScreenFonts" => USES_SCREEN_FONTS;
    NSDocumentTypeDocumentOption = "DocumentType";
    NSDefaultAttributesDocumentOption = "DefaultAttributes";
    NSCharacterEncodingDocumentOption = "CharacterEncoding";
    NSTextEncodingNameDocumentOption = "TextEncodingName";
    NSBaseURLDocumentOption = "BaseURL" => BASE_URL;
    NSTimeoutDocumentOption = "Timeout";
    NSWebPreferencesDocumentOption = "WebPreferences";
    NSWebResourceLoadDelegateDocumentOption = "WebResourceLoadDelegate";
    NSTextSizeMultiplierDocumentOption = "TextSizeMultiplier";
    NSFileTypeDocumentOption = "UTI";
    NSTargetTextScalingDocumentOption = "TargetTextScaling";
    NSSourceTextScalingDocumentOption = "SourceTextScaling";
    NSTextKit1ListMarkerFormatDocumentOption = "TextKit1ListMarkerFormat";
}

// Errors, with Foundation's codes.
const FILE_READ_UNKNOWN: isize = 256;
const FILE_READ_CORRUPT: isize = 259;
const FILE_READ_NO_SUCH_FILE: isize = 260;
const FILE_READ_UNSUPPORTED_SCHEME: isize = 262;
const TEXT_READ_INAPPLICABLE_TYPE: isize = 65806;
const TEXT_WRITE_INAPPLICABLE_TYPE: isize = 66062;

// NSStringEncoding values.
const ASCII: u32 = 1;
const UTF8: u32 = 4;
const LATIN1: u32 = 5;
const UNICODE: u32 = 10;
const CP1251: u32 = 11;
const CP1252: u32 = 12;
const CP1253: u32 = 13;
const CP1254: u32 = 14;
const CP1250: u32 = 15;
const MAC_ROMAN: u32 = 30;
const UTF16_BE: u32 = 0x9000_0100;
const UTF16_LE: u32 = 0x9400_0100;

/// A document format, as `NSDocumentTypeDocumentOption` names it.
fn format_named(name: &str) -> Option<Format> {
    Some(match name {
        "NSPlainText" => Format::Plain,
        "NSRTF" => Format::Rtf,
        "NSRTFD" => Format::Rtfd,
        "NSHTML" => Format::Html,
        _ => return None,
    })
}

/// The format of data read without a type, as AppKit tells it.
fn sniff(data: &[u8]) -> Format {
    if rtfd::is_rtfd(data) {
        Format::Rtfd
    } else if rtf_read::is_rtf(data) {
        Format::Rtf
    } else if html_read::is_html(data) {
        Format::Html
    } else {
        Format::Plain
    }
}

/// The HTML encoding an `NSStringEncoding` names.
fn html_encoding(encoding: u32) -> Option<html_read::Encoding> {
    use html_read::Encoding as E;
    use tables::CodePage as P;
    Some(match encoding {
        UTF8 => E::Utf8,
        UNICODE | UTF16_LE => E::Utf16Le,
        UTF16_BE => E::Utf16Be,
        ASCII | LATIN1 | CP1252 => E::Page(P::Cp1252),
        CP1250 => E::Page(P::Cp1250),
        CP1251 => E::Page(P::Cp1251),
        CP1253 => E::Page(P::Cp1253),
        CP1254 => E::Page(P::Cp1254),
        MAC_ROMAN => E::Page(P::MacRoman),
        _ => return None,
    })
}

/// A document read, its attributes, or a Cocoa error code.
type Read = Result<(Retained<NSMutableAttributedString>, Retained<Dict>), isize>;

/// How the pasteboard reads HTML: UTF-8 where the bytes are, as Linux
/// programs write it without a `charset`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Data,
    Pasteboard,
}

/// Read `data` as `initWithData:options:documentAttributes:error:` does.
fn read(data: &[u8], options: Option<&Dict>, source: Source) -> Read {
    let named = convert::string_value(options, keys::DOCUMENT_TYPE);
    let format = match &named {
        Some(name) => format_named(name).ok_or(TEXT_READ_INAPPLICABLE_TYPE)?,
        None => sniff(data),
    };
    let encoding = convert::number_value(options, keys::CHARACTER_ENCODING).map(|e| e as i64 as u32);
    let (doc, plain_encoding) = match format {
        Format::Rtf => (read_rtf(data, &[])?, None),
        Format::Rtfd => {
            let package = rtfd::read(data).ok_or(FILE_READ_UNKNOWN)?;
            (read_rtf(package.rtf, &package.files)?, None)
        }
        Format::Html => {
            let explicit = encoding.and_then(html_encoding).or_else(|| {
                convert::string_value(options, keys::TEXT_ENCODING_NAME)
                    .and_then(|n| html_read::Encoding::from_label(&n))
            });
            let base_url = convert::value(options, keys::BASE_URL).and_then(|v| match v.downcast::<NSURL>() {
                Ok(url) => url.absoluteString().map(|s| s.to_string()),
                Err(v) => v.downcast::<NSString>().ok().map(|s| s.to_string()),
            });
            let fallback = if source == Source::Pasteboard && std::str::from_utf8(data).is_ok() {
                html_read::Encoding::Utf8
            } else {
                html_read::Options::default().fallback
            };
            (html_read::read_bytes(data, &html_read::Options { encoding: explicit, fallback, base_url }), None)
        }
        Format::Plain => {
            let (text, used) = decode_plain(data, encoding);
            let string = NSMutableAttributedString::from_nsstring(&NSString::from_str(&text));
            let defaults = convert::value(options, keys::DEFAULT_ATTRIBUTES).and_then(dictionary);
            let attrs = defaults.clone().unwrap_or_else(|| {
                let font = convert::make_font(&model::Font::named("Helvetica", model::Generic::Sans, 12.0));
                // SAFETY: the key is a constant string AppKit exports.
                let key = unsafe { objc2_app_kit::NSFontAttributeName };
                NSDictionary::from_slices(&[key], &[&*font as &AnyObject])
            });
            if !text.is_empty() {
                // SAFETY: an attribute dictionary over the whole text.
                unsafe { string.setAttributes_range(Some(&attrs), NSRange::new(0, string.length())) };
            }
            let mut dict =
                convert::document_attributes(&model::DocAttrs::default(), Format::Plain, Some(used as usize));
            if let Some(defaults) = defaults {
                dict = with_entry(&dict, keys::DEFAULT_ATTRIBUTES, &defaults);
            }
            dict = with_entry(&dict, keys::FILE_TYPE, &NSString::from_str("public.plain-text"));
            return Ok((string, dict));
        }
    };
    let string = convert::to_attributed(&doc);
    let attrs = convert::document_attributes(&doc.attrs, format, plain_encoding);
    Ok((string, attrs))
}

fn read_rtf(data: &[u8], files: &[(String, Vec<u8>)]) -> Result<Doc, isize> {
    rtf_read::read_with(data, files).map_err(|e| match e {
        rtf_read::Error::NotRtf => FILE_READ_UNKNOWN,
        rtf_read::Error::Truncated => FILE_READ_CORRUPT,
    })
}

/// `value` if it is a dictionary.
fn dictionary(value: Retained<AnyObject>) -> Option<Retained<Dict>> {
    // SAFETY: isKindOfClass: takes a class and answers a BOOL.
    let is: bool = unsafe { msg_send![&*value, isKindOfClass: <NSDictionary as ClassType>::class()] };
    // SAFETY: an NSDictionary, whose keys an attribute dictionary's user
    // takes as strings.
    is.then(|| unsafe { Retained::cast_unchecked(value) })
}

/// `dict` with `key` set to `value` (in place of any value it had).
fn with_entry(dict: &Dict, key: &str, value: &AnyObject) -> Retained<Dict> {
    let key = NSString::from_str(key);
    let mut names: Vec<Retained<NSString>> = dict.allKeys().iter().filter(|k| !k.isEqualToString(&key)).collect();
    let mut values: Vec<Retained<AnyObject>> = names.iter().filter_map(|k| dict.objectForKey(k)).collect();
    names.push(key);
    values.push(value.retain());
    let name_refs: Vec<&NSString> = names.iter().map(|n| &**n).collect();
    let value_refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
    NSDictionary::from_slices(&name_refs, &value_refs)
}

/// Plain text's characters and the encoding they were read in: the one
/// asked for, else UTF-8 (UTF-16 after its byte order mark), else Mac OS
/// Roman, as AppKit reads plain text.
fn decode_plain(data: &[u8], encoding: Option<u32>) -> (String, u32) {
    let page = |p: tables::CodePage, e: u32| (data.iter().map(|&b| p.decode(b)).collect::<String>(), e);
    let utf16 = |data: &[u8], le: bool| -> String {
        let units =
            data.as_chunks::<2>().0.iter().map(|&c| if le { u16::from_le_bytes(c) } else { u16::from_be_bytes(c) });
        char::decode_utf16(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
    };
    match encoding {
        Some(UTF8) => {
            return (String::from_utf8_lossy(data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data)).into_owned(), UTF8);
        }
        Some(UNICODE) => {
            return match data {
                [0xFF, 0xFE, rest @ ..] => (utf16(rest, true), UNICODE),
                [0xFE, 0xFF, rest @ ..] => (utf16(rest, false), UNICODE),
                _ => (utf16(data, false), UNICODE),
            };
        }
        Some(UTF16_LE) => return (utf16(data, true), UTF16_LE),
        Some(UTF16_BE) => return (utf16(data, false), UTF16_BE),
        Some(LATIN1) => return page(tables::CodePage::Latin1, LATIN1),
        Some(e) => {
            if let Some(html_read::Encoding::Page(p)) = html_encoding(e) {
                return page(p, e);
            }
        }
        None => {}
    }
    if let [0xFF, 0xFE, rest @ ..] = data {
        return (utf16(rest, true), UNICODE);
    }
    if let [0xFE, 0xFF, rest @ ..] = data {
        return (utf16(rest, false), UNICODE);
    }
    match std::str::from_utf8(data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data)) {
        Ok(s) => (s.to_owned(), UTF8),
        Err(_) => page(tables::CodePage::MacRoman, MAC_ROMAN),
    }
}

/// Plain text in an encoding: UTF-8 unless asked otherwise (UTF-16 with a
/// byte order mark, little-endian; single-byte pages with `?` for what they
/// lack).
fn encode_plain(text: &str, encoding: Option<u32>) -> Vec<u8> {
    let single = |p: tables::CodePage| text.chars().map(|c| p.encode(c).unwrap_or(b'?')).collect();
    match encoding {
        None | Some(UTF8) => text.as_bytes().to_vec(),
        Some(UNICODE) => {
            let mut out = vec![0xFF, 0xFE];
            out.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            out
        }
        Some(UTF16_LE) => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        Some(UTF16_BE) => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        Some(ASCII) => text.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect(),
        Some(LATIN1) => text.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect(),
        Some(e) => match html_encoding(e) {
            Some(html_read::Encoding::Page(p)) => single(p),
            _ => text.as_bytes().to_vec(),
        },
    }
}

/// `range` of `string` in `format`.
fn write(string: &NSAttributedString, range: NSRange, format: Format, attrs: Option<&Dict>) -> Vec<u8> {
    let mut doc = convert::from_attributed(string, range);
    doc.attrs = convert::doc_attrs_of(attrs);
    match format {
        Format::Rtf => rtf_write::write(&doc),
        Format::Rtfd => {
            let files: Vec<(String, Vec<u8>)> =
                doc.attachments.iter().map(|a| (a.name.clone(), a.contents.clone())).collect();
            rtfd::write(&rtf_write::write_rtfd(&doc), &files, now())
        }
        Format::Html => html_write::write(&doc),
        Format::Plain => {
            let encoding = convert::number_value(attrs, keys::CHARACTER_ENCODING).map(|e| e as i64 as u32);
            encode_plain(&doc.text, encoding)
        }
    }
}

fn now() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32)
}

fn check_range(string: &NSAttributedString, range: NSRange, method: &str) {
    let len = string.length();
    if range.location.checked_add(range.length).is_none_or(|end| end > len) {
        panic!(
            "-[NSAttributedString {method}]: Range {{{}, {}}} out of bounds; string length {len}",
            range.location, range.length
        );
    }
}

/// Put an autoreleased error in `out`, if there is one.
fn set_error(out: *mut *mut NSError, code: isize) {
    if out.is_null() {
        return;
    }
    // SAFETY: the domain is Foundation's constant.
    let error = NSError::new(code, unsafe { NSCocoaErrorDomain });
    // SAFETY: the caller passes a valid pointer, which takes an
    // autoreleased error.
    unsafe { *out = Retained::autorelease_ptr(error) };
}

/// Put autoreleased document attributes in `out`, if there is one.
fn set_attributes(out: *mut *mut Dict, attrs: Retained<Dict>) {
    if !out.is_null() {
        // SAFETY: as for `set_error`.
        unsafe { *out = Retained::autorelease_ptr(attrs) };
    }
}

fn data_of(data: &NSData) -> Vec<u8> {
    // SAFETY: the bytes are copied before anything else runs.
    unsafe { sidestep_foundation::data::bytes(data) }.to_vec()
}

/// The receiver of a category method on `NSAttributedString`: an
/// attributed string.
pub(crate) fn receiver<T>(helper: &T) -> &NSAttributedString {
    // SAFETY: these methods are installed on NSAttributedString and only
    // ever run with an attributed string as the receiver.
    unsafe { &*(helper as *const T).cast::<NSAttributedString>() }
}

/// Initialize `this`, an attributed string being made, with `read`'s
/// result: its text and attributes, through its class's own
/// `initWithAttributedString:` (so a subclass's storage is set up as it
/// sets it up). Nil, with `error` set, when reading failed.
fn init_with<T: ClassType>(
    this: Allocated<T>,
    read: Read,
    attrs: *mut *mut Dict,
    error: *mut *mut NSError,
) -> Option<Retained<T>> {
    let (string, dict) = match read {
        Ok(r) => r,
        Err(code) => {
            set_error(error, code);
            return None;
        }
    };
    // SAFETY: an allocated object is one whatever its static type; the
    // receiver is an attributed string.
    let this: Allocated<AnyObject> = unsafe { std::mem::transmute::<Allocated<T>, Allocated<AnyObject>>(this) };
    // SAFETY: NSAttributedString's initializer, taking an attributed
    // string.
    let done: Option<Retained<AnyObject>> = unsafe { msg_send![this, initWithAttributedString: &*string] };
    set_attributes(attrs, dict);
    // SAFETY: the initialized receiver.
    done.map(|d| unsafe { Retained::cast_unchecked(d) })
}

/// The bytes of the file a URL names.
fn file_data(url: &NSURL) -> Option<(Vec<u8>, Option<Format>)> {
    let path = url.path()?.to_string();
    let ext = std::path::Path::new(&path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    let format = match ext.as_deref() {
        Some("rtf") => Some(Format::Rtf),
        Some("rtfd") => Some(Format::Rtfd),
        Some("html" | "htm") => Some(Format::Html),
        Some("txt" | "text") => Some(Format::Plain),
        _ => None,
    };
    if format == Some(Format::Rtfd) && std::path::Path::new(&path).is_dir() {
        // An RTFD package: its TXT.rtf, and its other files for the
        // attachments the text names, read as the flat RTFD they'd make.
        let dir = std::path::Path::new(&path);
        let rtf = std::fs::read(dir.join("TXT.rtf")).ok()?;
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != "TXT.rtf" && entry.file_type().is_ok_and(|t| t.is_file()) {
                files.push((name, std::fs::read(entry.path()).ok()?));
            }
        }
        files.sort();
        return Some((rtfd::write(&rtf, &files, 0), Some(Format::Rtfd)));
    }
    std::fs::read(&path).ok().map(|d| (d, format))
}

/// Read a file as `initWithURL:options:documentAttributes:error:` does:
/// a file URL's (other schemes are `NSFileReadUnsupportedSchemeError`, as
/// AppKit has them).
fn read_url(url: &NSURL, options: Option<&Dict>) -> Read {
    if !url.isFileURL() {
        return Err(FILE_READ_UNSUPPORTED_SCHEME);
    }
    let (data, format) = file_data(url).ok_or(FILE_READ_NO_SUCH_FILE)?;
    if convert::value(options, keys::DOCUMENT_TYPE).is_some() {
        return read(&data, options, Source::Data);
    }
    let name = match format {
        Some(Format::Rtf) => "NSRTF",
        Some(Format::Rtfd) => "NSRTFD",
        Some(Format::Html) => "NSHTML",
        Some(Format::Plain) => "NSPlainText",
        None => return read(&data, options, Source::Data),
    };
    let typed =
        with_entry(options.map_or(&*NSDictionary::new(), |o| o), keys::DOCUMENT_TYPE, &NSString::from_str(name));
    read(&data, Some(&typed), Source::Data)
}

/// Options naming a document type.
fn typed(format: &str) -> Retained<Dict> {
    NSDictionary::from_slices(
        &[&*NSString::from_str(keys::DOCUMENT_TYPE)],
        &[&*NSString::from_str(format) as &AnyObject],
    )
}

/// An attributed string made from a pasteboard value of type `kind`.
pub(crate) fn from_pasteboard(value: &AnyObject, kind: &NSString) -> Option<Retained<NSMutableAttributedString>> {
    let kind = crate::pasteboard_types::from_ns(kind);
    if kind == STRING_TYPE || kind == "public.plain-text" || kind == "public.text" {
        let string = match value.downcast_ref::<NSString>() {
            Some(s) => s.retain(),
            None => NSString::from_str(&String::from_utf8_lossy(&data_of(value.downcast_ref::<NSData>()?))),
        };
        return Some(NSMutableAttributedString::from_nsstring(&string));
    }
    let bytes = match (value.downcast_ref::<NSData>(), value.downcast_ref::<NSString>()) {
        (Some(d), _) => data_of(d),
        (None, Some(s)) => s.to_string().into_bytes(),
        _ => return None,
    };
    let format = match kind.as_str() {
        RTF_TYPE => "NSRTF",
        RTFD_TYPE => "NSRTFD",
        HTML_TYPE => "NSHTML",
        _ => return None,
    };
    read(&bytes, Some(&typed(format)), Source::Pasteboard).ok().map(|(s, _)| s)
}

/// `range` of `string` as a pasteboard type's value (RTF, RTFD and HTML as
/// data, text as a string).
pub(crate) fn to_pasteboard(string: &NSAttributedString, range: NSRange, kind: &str) -> Option<Retained<AnyObject>> {
    let format = match kind {
        RTF_TYPE => Format::Rtf,
        RTFD_TYPE => Format::Rtfd,
        HTML_TYPE => Format::Html,
        STRING_TYPE => {
            let text = string.string().substringWithRange(range);
            return Some(any(text));
        }
        _ => return None,
    };
    Some(any(NSData::with_bytes(&write(string, range, format, None))))
}

/// Whether `range` of `string` has attachments.
fn has_attachments(string: &NSAttributedString, range: NSRange) -> bool {
    // SAFETY: the key is a constant string AppKit exports.
    let key = unsafe { objc2_app_kit::NSAttachmentAttributeName };
    let mut at = range.location;
    let end = range.location + range.length;
    while at < end {
        let mut found = NSRange::new(0, 0);
        // SAFETY: an index inside the string, and a range to fill.
        let value: Option<Retained<AnyObject>> =
            unsafe { msg_send![string, attribute: key, atIndex: at, effectiveRange: &mut found] };
        if value.is_some() {
            return true;
        }
        at = (found.location + found.length).max(at + 1);
    }
    false
}

fn strings(names: &[&str]) -> Retained<NSArray<NSString>> {
    let items: Vec<Retained<NSString>> = names.iter().map(|n| NSString::from_str(n)).collect();
    NSArray::from_retained_slice(&items)
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepAttributedStringRichText"]
    struct RichText;

    impl RichText {
        #[unsafe(method_id(initWithData:options:documentAttributes:error:))]
        fn init_with_data(
            this: Allocated<Self>,
            data: &NSData,
            options: Option<&Dict>,
            attrs: *mut *mut Dict,
            error: *mut *mut NSError,
        ) -> Option<Retained<Self>> {
            init_with(this, read(&data_of(data), options, Source::Data), attrs, error)
        }

        #[unsafe(method_id(initWithURL:options:documentAttributes:error:))]
        fn init_with_url(
            this: Allocated<Self>,
            url: &NSURL,
            options: Option<&Dict>,
            attrs: *mut *mut Dict,
            error: *mut *mut NSError,
        ) -> Option<Retained<Self>> {
            init_with(this, read_url(url, options), attrs, error)
        }

        #[unsafe(method_id(initWithURL:documentAttributes:))]
        fn init_with_url_attributes(this: Allocated<Self>, url: &NSURL, attrs: *mut *mut Dict) -> Option<Retained<Self>> {
            init_with(this, read_url(url, None), attrs, std::ptr::null_mut())
        }

        #[unsafe(method_id(initWithPath:documentAttributes:))]
        fn init_with_path(this: Allocated<Self>, path: &NSString, attrs: *mut *mut Dict) -> Option<Retained<Self>> {
            let url = NSURL::fileURLWithPath(path);
            init_with(this, read_url(&url, None), attrs, std::ptr::null_mut())
        }

        #[unsafe(method_id(initWithRTF:documentAttributes:))]
        fn init_with_rtf(this: Allocated<Self>, data: &NSData, attrs: *mut *mut Dict) -> Option<Retained<Self>> {
            init_with(this, read(&data_of(data), Some(&typed("NSRTF")), Source::Data), attrs, std::ptr::null_mut())
        }

        #[unsafe(method_id(initWithRTFD:documentAttributes:))]
        fn init_with_rtfd(this: Allocated<Self>, data: &NSData, attrs: *mut *mut Dict) -> Option<Retained<Self>> {
            init_with(this, read(&data_of(data), Some(&typed("NSRTFD")), Source::Data), attrs, std::ptr::null_mut())
        }

        #[unsafe(method_id(initWithHTML:documentAttributes:))]
        fn init_with_html(this: Allocated<Self>, data: &NSData, attrs: *mut *mut Dict) -> Option<Retained<Self>> {
            init_with(this, read(&data_of(data), Some(&typed("NSHTML")), Source::Data), attrs, std::ptr::null_mut())
        }

        #[unsafe(method_id(initWithHTML:baseURL:documentAttributes:))]
        fn init_with_html_base(
            this: Allocated<Self>,
            data: &NSData,
            base: Option<&NSURL>,
            attrs: *mut *mut Dict,
        ) -> Option<Retained<Self>> {
            let mut names = vec![NSString::from_str(keys::DOCUMENT_TYPE)];
            let mut values: Vec<Retained<AnyObject>> = vec![any(NSString::from_str("NSHTML"))];
            if let Some(base) = base {
                names.push(NSString::from_str(keys::BASE_URL));
                values.push(any(base.retain()));
            }
            let name_refs: Vec<&NSString> = names.iter().map(|n| &**n).collect();
            let value_refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
            let options = NSDictionary::from_slices(&name_refs, &value_refs);
            init_with(this, read(&data_of(data), Some(&options), Source::Data), attrs, std::ptr::null_mut())
        }

        #[unsafe(method_id(initWithHTML:options:documentAttributes:))]
        fn init_with_html_options(
            this: Allocated<Self>,
            data: &NSData,
            options: Option<&Dict>,
            attrs: *mut *mut Dict,
        ) -> Option<Retained<Self>> {
            let options = with_entry(options.map_or(&*NSDictionary::new(), |o| o), keys::DOCUMENT_TYPE, &NSString::from_str("NSHTML"));
            init_with(this, read(&data_of(data), Some(&options), Source::Data), attrs, std::ptr::null_mut())
        }

        #[unsafe(method_id(dataFromRange:documentAttributes:error:))]
        fn data_from_range(&self, range: NSRange, attrs: Option<&Dict>, error: *mut *mut NSError) -> Option<Retained<NSData>> {
            let string = receiver(self);
            check_range(string, range, "dataFromRange:documentAttributes:error:");
            let format = convert::string_value(attrs, keys::DOCUMENT_TYPE).and_then(|n| format_named(&n));
            match format {
                Some(format) => Some(NSData::with_bytes(&write(string, range, format, attrs))),
                None => {
                    set_error(error, TEXT_WRITE_INAPPLICABLE_TYPE);
                    None
                }
            }
        }

        #[unsafe(method_id(RTFFromRange:documentAttributes:))]
        fn rtf_from_range(&self, range: NSRange, attrs: Option<&Dict>) -> Option<Retained<NSData>> {
            let string = receiver(self);
            check_range(string, range, "RTFFromRange:documentAttributes:");
            Some(NSData::with_bytes(&write(string, range, Format::Rtf, attrs)))
        }

        #[unsafe(method_id(RTFDFromRange:documentAttributes:))]
        fn rtfd_from_range(&self, range: NSRange, attrs: Option<&Dict>) -> Option<Retained<NSData>> {
            let string = receiver(self);
            check_range(string, range, "RTFDFromRange:documentAttributes:");
            Some(NSData::with_bytes(&write(string, range, Format::Rtfd, attrs)))
        }

        #[unsafe(method(containsAttachmentsInRange:))]
        fn contains_attachments_in_range(&self, range: NSRange) -> bool {
            let string = receiver(self);
            check_range(string, range, "containsAttachmentsInRange:");
            has_attachments(string, range)
        }

        #[unsafe(method(containsAttachments))]
        fn contains_attachments(&self) -> bool {
            let string = receiver(self);
            has_attachments(string, NSRange::new(0, string.length()))
        }

        #[unsafe(method(prefersRTFDInRange:))]
        fn prefers_rtfd_in_range(&self, range: NSRange) -> bool {
            let string = receiver(self);
            check_range(string, range, "prefersRTFDInRange:");
            has_attachments(string, range)
        }

        #[unsafe(method_id(textTypes))]
        fn text_types() -> Retained<NSArray<NSString>> {
            strings(&["public.plain-text", "public.rtf", "com.apple.rtfd", "public.html"])
        }

        #[unsafe(method_id(textUnfilteredTypes))]
        fn text_unfiltered_types() -> Retained<NSArray<NSString>> {
            strings(&["public.plain-text", "public.rtf", "com.apple.rtfd", "public.html"])
        }

        // NSPasteboardReading.

        #[unsafe(method_id(readableTypesForPasteboard:))]
        fn readable_types(_pasteboard: Option<&AnyObject>) -> Retained<NSArray<NSString>> {
            strings(&[RTFD_TYPE, RTF_TYPE, HTML_TYPE, STRING_TYPE])
        }

        #[unsafe(method(readingOptionsForType:pasteboard:))]
        fn reading_options(kind: &NSString, _pasteboard: Option<&AnyObject>) -> usize {
            // Text is read as a string (NSPasteboardReadingAsString); the
            // rest as data.
            usize::from(crate::pasteboard_types::from_ns(kind) == STRING_TYPE)
        }

        #[unsafe(method_id(initWithPasteboardPropertyList:ofType:))]
        fn init_with_pasteboard(this: Allocated<Self>, list: &AnyObject, kind: &NSString) -> Option<Retained<Self>> {
            let read = from_pasteboard(list, kind).map(|s| (s, NSDictionary::new())).ok_or(FILE_READ_UNKNOWN);
            init_with(this, read, std::ptr::null_mut(), std::ptr::null_mut())
        }

        // NSPasteboardWriting.

        #[unsafe(method_id(writableTypesForPasteboard:))]
        fn writable_types(&self, _pasteboard: Option<&AnyObject>) -> Retained<NSArray<NSString>> {
            let string = receiver(self);
            if has_attachments(string, NSRange::new(0, string.length())) {
                strings(&[RTFD_TYPE, RTF_TYPE, HTML_TYPE, STRING_TYPE])
            } else {
                strings(&[RTF_TYPE, HTML_TYPE, STRING_TYPE])
            }
        }

        #[unsafe(method(writingOptionsForType:pasteboard:))]
        fn writing_options(&self, _kind: &NSString, _pasteboard: Option<&AnyObject>) -> usize {
            0
        }

        #[unsafe(method_id(pasteboardPropertyListForType:))]
        fn property_list(&self, kind: &NSString) -> Option<Retained<AnyObject>> {
            let string = receiver(self);
            to_pasteboard(string, NSRange::new(0, string.length()), &crate::pasteboard_types::from_ns(kind))
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepMutableAttributedStringRichText"]
    struct MutableRichText;

    impl MutableRichText {
        #[unsafe(method(readFromData:options:documentAttributes:error:))]
        fn read_from_data(&self, data: &NSData, options: Option<&Dict>, attrs: *mut *mut Dict, error: *mut *mut NSError) -> bool {
            self.replace(read(&data_of(data), options, Source::Data), attrs, error)
        }

        #[unsafe(method(readFromURL:options:documentAttributes:error:))]
        fn read_from_url(&self, url: &NSURL, options: Option<&Dict>, attrs: *mut *mut Dict, error: *mut *mut NSError) -> bool {
            self.replace(read_url(url, options), attrs, error)
        }
    }
);

impl MutableRichText {
    fn replace(&self, read: Read, attrs: *mut *mut Dict, error: *mut *mut NSError) -> bool {
        // SAFETY: installed on NSMutableAttributedString.
        let this = unsafe { &*(self as *const Self).cast::<NSMutableAttributedString>() };
        match read {
            Ok((string, dict)) => {
                // RTF (and RTFD) are read onto the end of what the string
                // holds, as AppKit's RTF reader does; the other formats
                // replace it.
                let kind = convert::string_value(Some(&dict), keys::DOCUMENT_TYPE);
                if matches!(kind.as_deref(), Some("NSRTF" | "NSRTFD")) {
                    this.appendAttributedString(&string);
                } else {
                    this.setAttributedString(&string);
                }
                set_attributes(attrs, dict);
                true
            }
            Err(code) => {
                set_error(error, code);
                false
            }
        }
    }
}

// AppKit's additions to the attributed string classes, attached when
// Foundation's classes register.
sidestep_runtime::category!("NSAttributedString"(NSAttributedStringKitAdditions), |category| {
    // SAFETY: the helper's methods treat their receiver as an attributed
    // string (and its class methods their receiver as the class).
    unsafe { category.add_methods_of(RichText::class()) };
});

sidestep_runtime::category!("NSMutableAttributedString"(NSMutableAttributedStringKitAdditions), |category| {
    // SAFETY: the helper's methods treat their receiver as a mutable
    // attributed string.
    unsafe { category.add_methods_of(MutableRichText::class()) };
});

#[cfg(test)]
mod tests {
    use objc2::AnyThread;
    use objc2_app_kit::{
        NSAttributedStringAppKitDocumentFormats, NSAttributedStringDocumentFormats, NSColor, NSFont,
        NSFontAttributeName, NSFontDescriptorSymbolicTraits, NSForegroundColorAttributeName, NSLinkAttributeName,
        NSMutableParagraphStyle, NSParagraphStyle, NSParagraphStyleAttributeName, NSPasteboard, NSPasteboardReading,
        NSTextAlignment,
    };
    use objc2_foundation::ns_string;

    use super::*;

    fn attrs(pairs: &[(&NSString, &AnyObject)]) -> Retained<Dict> {
        let keys: Vec<&NSString> = pairs.iter().map(|p| p.0).collect();
        let values: Vec<&AnyObject> = pairs.iter().map(|p| p.1).collect();
        NSDictionary::from_slices(&keys, &values)
    }

    /// "Plain Bold red\nCentered" with a bold font, a red color, a link and
    /// a centered second paragraph.
    fn sample() -> Retained<NSMutableAttributedString> {
        let s = NSMutableAttributedString::from_nsstring(ns_string!("Plain Bold red\nCentered"));
        let helvetica = NSFont::fontWithName_size(ns_string!("Helvetica"), 12.0).unwrap();
        let bold = NSFont::fontWithName_size(ns_string!("Helvetica-Bold"), 12.0)
            .unwrap_or_else(|| NSFont::boldSystemFontOfSize(12.0));
        let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
        let centered = NSMutableParagraphStyle::new();
        centered.setAlignment(NSTextAlignment::Center);
        let link = NSURL::URLWithString(ns_string!("https://example.com/a")).unwrap();
        // SAFETY: attribute dictionaries of the right kinds, over ranges in
        // the text.
        unsafe {
            s.setAttributes_range(Some(&attrs(&[(NSFontAttributeName, &helvetica)])), NSRange::new(0, s.length()));
            s.addAttribute_value_range(NSFontAttributeName, &bold, NSRange::new(6, 4));
            s.addAttribute_value_range(NSForegroundColorAttributeName, &red, NSRange::new(11, 3));
            s.addAttribute_value_range(NSLinkAttributeName, &link, NSRange::new(0, 5));
            s.addAttribute_value_range(NSParagraphStyleAttributeName, &centered, NSRange::new(15, 8));
        }
        s
    }

    fn check_sample(back: &NSAttributedString) {
        // (HTML's last paragraph ends in a newline, as AppKit reads it.)
        assert_eq!(back.string().to_string().trim_end_matches('\n'), "Plain Bold red\nCentered");
        let at = |i: usize, key: &NSString| {
            // SAFETY: an index inside the string.
            unsafe { back.attribute_atIndex_effectiveRange(key, i, std::ptr::null_mut()) }
        };
        // SAFETY: constant keys.
        let (font, color, style, link) = unsafe {
            (NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName, NSLinkAttributeName)
        };
        let bold = at(7, font).unwrap().downcast::<NSFont>().unwrap();
        assert!(bold.fontDescriptor().symbolicTraits().contains(NSFontDescriptorSymbolicTraits::TraitBold));
        let plain = at(1, font).unwrap().downcast::<NSFont>().unwrap();
        assert!(!plain.fontDescriptor().symbolicTraits().contains(NSFontDescriptorSymbolicTraits::TraitBold));
        let red = at(12, color).unwrap().downcast::<NSColor>().unwrap();
        let red = red.colorUsingColorSpace(&objc2_app_kit::NSColorSpace::sRGBColorSpace()).unwrap();
        assert!((red.redComponent() - 1.0).abs() < 0.01 && red.greenComponent() < 0.01);
        assert!(at(5, color).is_none(), "no color outside the red and the link");
        let centered = at(16, style).unwrap().downcast::<NSParagraphStyle>().unwrap();
        assert_eq!(centered.alignment(), NSTextAlignment::Center);
        let url = at(2, link).unwrap().downcast::<NSURL>().unwrap();
        assert_eq!(url.absoluteString().unwrap().to_string(), "https://example.com/a");
    }

    #[test]
    fn rtf_and_rtfd_round_trip() {
        let s = sample();
        // SAFETY: an empty attribute dictionary.
        let rtf =
            unsafe { s.RTFFromRange_documentAttributes(NSRange::new(0, s.length()), &NSDictionary::new()) }.unwrap();
        let text = String::from_utf8(data_of(&rtf)).unwrap();
        assert!(text.starts_with("{\\rtf1\\ansi\\ansicpg1252\\cocoartf"), "{text}");
        let mut doc_attrs = None;
        // SAFETY: RTF data, and a place for the attributes.
        let back = unsafe {
            NSAttributedString::initWithRTF_documentAttributes(NSAttributedString::alloc(), &rtf, Some(&mut doc_attrs))
        }
        .expect("RTF reads");
        check_sample(&back);
        let doc_attrs = doc_attrs.unwrap();
        assert_eq!(convert::string_value(Some(&doc_attrs), keys::DOCUMENT_TYPE).as_deref(), Some("NSRTF"));
        assert_eq!(convert::number_value(Some(&doc_attrs), keys::LEFT_MARGIN), Some(90.0));
        // SAFETY: as above.
        let rtfd =
            unsafe { s.RTFDFromRange_documentAttributes(NSRange::new(0, s.length()), &NSDictionary::new()) }.unwrap();
        assert!(rtfd::is_rtfd(&data_of(&rtfd)));
        // SAFETY: RTFD data.
        let back =
            unsafe { NSAttributedString::initWithRTFD_documentAttributes(NSAttributedString::alloc(), &rtfd, None) }
                .unwrap();
        check_sample(&back);
        // Garbage makes nothing.
        // SAFETY: data.
        let none = unsafe {
            NSAttributedString::initWithRTF_documentAttributes(
                NSAttributedString::alloc(),
                &NSData::with_bytes(b"nope"),
                None,
            )
        };
        assert!(none.is_none());
    }

    /// Readers never panic, whatever they are given: random bytes and
    /// random pieces of their syntax.
    #[test]
    fn readers_take_anything() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let rtf_pieces: &[&[u8]] = &[
            b"{",
            b"}",
            b"\\",
            b"\\'e9",
            b"\\'",
            b"\\u-10179 ",
            b"\\u56832?",
            b"\\uc2",
            b"\\par ",
            b"\\pard",
            b"\\b",
            b"\\f1 ",
            b"\\fonttbl",
            b"\\colortbl;",
            b"\\red255",
            b";",
            b"\\*",
            b"\\field",
            b"\\fldinst HYPERLINK \"x",
            b"\\fldrslt ",
            b"\\bin5 ",
            b"\\tx100",
            b"\\tqr",
            b"\\ri-9999999999",
            b"text ",
            b"\xff\xfe",
            b"\\cf99 ",
            b"\\expandedcolortbl",
            b"\\cssrgb\\c100000",
            b"\\cname x",
            b"\\NeXTGraphic",
            b"\\row",
            b"\\cell",
            b"\\line",
            // Numbers at and past the ends of their range.
            b"\\li2147483647",
            b"\\fi1",
            b"\\fi-2147483648",
            b"\\sl-2147483648 ",
            b"\\li99999999999999999999",
            b"\\u99999999999 ",
            b"\\uc2147483647",
            b"\\bin99999999999",
            b"\\fs2147483647 ",
            b"\\deff5",
            b"\\plain ",
        ];
        let html_pieces: &[&str] = &[
            "<p>",
            "</p>",
            "<b>",
            "</i>",
            "<br>",
            "<pre>\n",
            "</pre>",
            "<ul><li>",
            "<ol start=x>",
            "<li value=3>",
            "&amp;",
            "&#x1F600;",
            "&#99999999;",
            "&",
            "<",
            ">",
            "<a href=\"../x\">",
            "<style>p{color:red",
            "</style>",
            "<!--",
            "<span style=\"font: bold; margin: ; color: rgb(\">",
            "<h7>",
            "<table><tr><td>",
            "text ",
            "é",
            "<br/>",
            "<hr>",
            "<font size=+9 color=#12>",
            "<p style=\"text-indent:1e400px\">",
            "</ul>",
            "<base href=\"x:\">",
            "<script>",
            // Numbers at and past the ends of their range, and blocks in
            // list items.
            "<font size=\"+9223372036854775807\">",
            "<font size=\"-2147483648\">",
            "<ol start=\"9223372036854775807\">",
            "<ol type=i start=2147483647>",
            "<ol type=A start=-2147483648>",
            "<li value=99999999999999>",
            "<p style=\"margin-left:1e308px; text-indent:-1e308px\">",
            "<li><p>",
            "<div>",
            "</div>",
            "</li>",
            "</ol>",
            "<span class=\"Apple-converted-space\">&nbsp;",
            "<body>",
            "</body>",
            "<pre>a\nb",
        ];
        // 400 rounds; SIDESTEP_RICH_ROUNDS asks for more.
        let rounds = std::env::var("SIDESTEP_RICH_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
        for round in 0..rounds {
            let mut rtf = b"{\\rtf1".to_vec();
            let mut html = String::new();
            let mut raw = Vec::new();
            for _ in 0..(next() % 60) {
                rtf.extend_from_slice(rtf_pieces[(next() % rtf_pieces.len() as u64) as usize]);
                html.push_str(html_pieces[(next() % html_pieces.len() as u64) as usize]);
                raw.push(next() as u8);
            }
            if round % 2 == 0 {
                rtf.push(b'}');
            }
            let _ = rtf_read::read(&rtf);
            let _ = html_read::read(&html, &html_read::Options::default());
            let _ = html_read::read_bytes(&raw, &html_read::Options::default());
            let _ = rtfd::read(&[b"rtfd\0\0\0\0".as_slice(), &raw].concat());
            if let Ok(doc) = rtf_read::read(&rtf) {
                // What was read is written and read again.
                let again = rtf_read::read(&rtf_write::write(&doc)).expect("written RTF reads");
                let norm = |t: &str| t.replace("\r\n", "\n").replace(['\r', '\u{2029}'], "\n");
                assert_eq!(norm(&again.text), norm(&doc.text), "{:?}", String::from_utf8_lossy(&rtf));
                let _ = html_read::read_bytes(&html_write::write(&doc), &html_read::Options::default());
                let _ = convert::to_attributed(&doc);
            }
        }
    }

    #[test]
    fn text_storages_read_documents() {
        use objc2_app_kit::NSTextStorage;
        let s = sample();
        // SAFETY: an empty attribute dictionary.
        let rtf =
            unsafe { s.RTFFromRange_documentAttributes(NSRange::new(0, s.length()), &NSDictionary::new()) }.unwrap();
        // SAFETY: the initializer takes RTF data and a place for the
        // attributes.
        let storage: Option<Retained<NSTextStorage>> = unsafe {
            msg_send![NSTextStorage::alloc(), initWithRTF: &*rtf, documentAttributes: std::ptr::null_mut::<*mut Dict>()]
        };
        check_sample(&storage.unwrap());
        // Read in place: RTF onto the end, as AppKit's RTF reader does.
        let storage = NSTextStorage::new();
        storage.replaceCharactersInRange_withString(NSRange::new(0, 0), ns_string!("old "));
        let data = NSData::with_bytes(b"{\\rtf1 new}");
        // SAFETY: RTF data, options, and no places for attributes or an
        // error.
        let ok: bool = unsafe {
            msg_send![&*storage, readFromData: &*data, options: &*typed("NSRTF"), documentAttributes: std::ptr::null_mut::<*mut Dict>(), error: std::ptr::null_mut::<*mut NSError>()]
        };
        assert!(ok);
        assert_eq!(storage.string().to_string(), "old new");
    }

    #[test]
    fn documents_by_type() {
        let s = sample();
        let range = NSRange::new(0, s.length());
        let of_type = |t: &str| typed(t);
        for t in ["NSRTF", "NSRTFD", "NSHTML", "NSPlainText"] {
            // SAFETY: attribute dictionaries.
            let data = unsafe { s.dataFromRange_documentAttributes_error(range, &of_type(t)) }.expect(t);
            // SAFETY: the options name the type.
            let back = unsafe {
                NSAttributedString::initWithData_options_documentAttributes_error(
                    NSAttributedString::alloc(),
                    &data,
                    &of_type(t),
                    None,
                )
            }
            .expect(t);
            assert_eq!(back.string().to_string().trim_end(), "Plain Bold red\nCentered", "{t}");
            if t != "NSPlainText" {
                check_sample(&back);
            }
        }
        // No type, or one Sidestep doesn't write: an error.
        // SAFETY: as above.
        let e = unsafe { s.dataFromRange_documentAttributes_error(range, &NSDictionary::new()) }.unwrap_err();
        assert_eq!((e.code(), e.domain().to_string()), (66062, "NSCocoaErrorDomain".to_string()));
        // SAFETY: as above.
        let e = unsafe {
            NSAttributedString::initWithData_options_documentAttributes_error(
                NSAttributedString::alloc(),
                &NSData::with_bytes(b"x"),
                &of_type("bogus"),
                None,
            )
        }
        .unwrap_err();
        assert_eq!(e.code(), 65806);
        // SAFETY: as above.
        let e = unsafe {
            NSAttributedString::initWithData_options_documentAttributes_error(
                NSAttributedString::alloc(),
                &NSData::with_bytes(b"{\\rtf1 {\\b x"),
                &of_type("NSRTF"),
                None,
            )
        }
        .unwrap_err();
        assert_eq!(e.code(), 259);
        // Without a type, the data says: RTF, HTML or plain text.
        for (data, kind) in
            [(&b"{\\rtf1 x}"[..], "NSRTF"), (b"<html><body>x</body></html>", "NSHTML"), (b"<b>x</b>", "NSPlainText")]
        {
            let mut doc_attrs = None;
            // SAFETY: as above.
            let _ = unsafe {
                NSAttributedString::initWithData_options_documentAttributes_error(
                    NSAttributedString::alloc(),
                    &NSData::with_bytes(data),
                    &NSDictionary::new(),
                    Some(&mut doc_attrs),
                )
            }
            .unwrap();
            assert_eq!(convert::string_value(doc_attrs.as_deref(), keys::DOCUMENT_TYPE).as_deref(), Some(kind));
        }
    }

    #[test]
    fn plain_text_takes_default_attributes() {
        let font = NSFont::fontWithName_size(ns_string!("Helvetica"), 20.0).unwrap();
        // SAFETY: a constant key.
        let defaults = attrs(&[(unsafe { NSFontAttributeName }, &font)]);
        let options = NSDictionary::from_slices(
            &[&*NSString::from_str(keys::DOCUMENT_TYPE), &*NSString::from_str(keys::DEFAULT_ATTRIBUTES)],
            &[&*NSString::from_str("NSPlainText") as &AnyObject, &*defaults],
        );
        let mut doc_attrs = None;
        // SAFETY: the options' values are of the right kinds.
        let s = unsafe {
            NSAttributedString::initWithData_options_documentAttributes_error(
                NSAttributedString::alloc(),
                &NSData::with_bytes("café".as_bytes()),
                &options,
                Some(&mut doc_attrs),
            )
        }
        .unwrap();
        assert_eq!(s.string().to_string(), "café");
        // SAFETY: an index in the string and a constant key.
        let got = unsafe { s.attribute_atIndex_effectiveRange(NSFontAttributeName, 0, std::ptr::null_mut()) }.unwrap();
        assert_eq!(got.downcast::<NSFont>().unwrap().pointSize(), 20.0);
        assert_eq!(convert::number_value(doc_attrs.as_deref(), keys::CHARACTER_ENCODING), Some(4.0));
        // Not UTF-8: Mac OS Roman.
        assert_eq!(decode_plain(&[0x63, 0xe9], None), ("cÈ".to_string(), MAC_ROMAN));
        assert_eq!(encode_plain("hi", Some(UNICODE)), [0xff, 0xfe, b'h', 0, b'i', 0]);
    }

    #[test]
    fn pasteboards_take_attributed_strings() {
        let pb = NSPasteboard::pasteboardWithUniqueName();
        pb.clearContents();
        let s = sample();
        let objects = NSArray::from_slice(&[&*s as &AnyObject]);
        // SAFETY: an array of objects that write themselves.
        assert!(unsafe { msg_send![&*pb, writeObjects: &*objects] });
        let types: Vec<String> = pb.types().unwrap().iter().map(|t| t.to_string()).collect();
        assert!(types.iter().position(|t| t == RTF_TYPE) < types.iter().position(|t| t == STRING_TYPE), "{types:?}");
        assert!(types.iter().any(|t| t == HTML_TYPE), "{types:?}");
        let classes = NSArray::from_slice(&[<NSAttributedString as ClassType>::class()]);
        // SAFETY: an array of classes that read themselves.
        let read: Option<Retained<NSArray<AnyObject>>> =
            unsafe { msg_send![&*pb, readObjectsForClasses: &*classes, options: std::ptr::null::<AnyObject>()] };
        let read = read.unwrap();
        let back = read.firstObject().unwrap().downcast::<NSAttributedString>().unwrap();
        check_sample(&back);
        // Text alone reads as an attributed string with no attributes; HTML
        // without a charset reads as UTF-8 when it is.
        pb.clearContents();
        // SAFETY: the type is a constant string.
        pb.setString_forType(ns_string!("plain"), unsafe { objc2_app_kit::NSPasteboardTypeString });
        // SAFETY: as above.
        let read: Retained<NSArray<AnyObject>> =
            unsafe { msg_send![&*pb, readObjectsForClasses: &*classes, options: std::ptr::null::<AnyObject>()] };
        let plain = read.firstObject().unwrap().downcast::<NSAttributedString>().unwrap();
        assert_eq!(plain.string().to_string(), "plain");
        // SAFETY: an index in the string.
        assert_eq!(unsafe { plain.attributesAtIndex_effectiveRange(0, std::ptr::null_mut()) }.count(), 0);
        pb.clearContents();
        // SAFETY: data for a constant type.
        pb.setData_forType(Some(&NSData::with_bytes("<p>caf\u{e9} <b>b</b></p>".as_bytes())), unsafe {
            objc2_app_kit::NSPasteboardTypeHTML
        });
        // SAFETY: as above.
        let read: Retained<NSArray<AnyObject>> =
            unsafe { msg_send![&*pb, readObjectsForClasses: &*classes, options: std::ptr::null::<AnyObject>()] };
        let html = read.firstObject().unwrap().downcast::<NSAttributedString>().unwrap();
        assert_eq!(html.string().to_string(), "café b\n");
        let readable = NSAttributedString::readableTypesForPasteboard(&pb);
        assert_eq!(
            readable.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
            [RTFD_TYPE, RTF_TYPE, HTML_TYPE, STRING_TYPE]
        );
        // SAFETY: a pasteboard's method, taking nothing.
        let _: () = unsafe { msg_send![&*pb, releaseGlobally] };
    }

    /// Options for HTML name HTML, whatever the caller's said; other schemes
    /// than `file:` aren't read.
    #[test]
    fn html_options_and_urls() {
        let rtf_typed = typed("NSRTF");
        // SAFETY: HTML data, options, and no place for attributes.
        let s = unsafe {
            NSAttributedString::initWithHTML_options_documentAttributes(
                NSAttributedString::alloc(),
                &NSData::with_bytes(b"<b>x</b>"),
                &rtf_typed,
                None,
            )
        };
        assert_eq!(s.map(|s| s.string().to_string()).as_deref(), Some("x"));
        let replaced = with_entry(&rtf_typed, keys::DOCUMENT_TYPE, &NSString::from_str("NSHTML"));
        assert_eq!(replaced.count(), 1);
        assert_eq!(convert::string_value(Some(&replaced), keys::DOCUMENT_TYPE).as_deref(), Some("NSHTML"));
        let path = std::env::temp_dir().join(format!("sidestep-rich-{}.txt", std::process::id()));
        std::fs::write(&path, "local").unwrap();
        let file = NSURL::from_file_path(&path).unwrap();
        let remote =
            NSURL::URLWithString(&NSString::from_str(&format!("https://example.invalid{}", path.display()))).unwrap();
        assert_eq!(read_url(&file, None).map(|(s, _)| s.string().to_string()), Ok("local".to_string()));
        assert_eq!(read_url(&remote, None).map(|(s, _)| s.string().to_string()), Err(262));
        let _ = std::fs::remove_file(&path);
    }
}
