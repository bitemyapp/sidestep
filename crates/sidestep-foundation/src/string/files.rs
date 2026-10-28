//! A string's text read from a file, or written to one:
//! `-initWithContentsOfURL:encoding:error:` and its `+stringWith…` and
//! path forms, the forms that work out the encoding (`usedEncoding:`), and
//! `-writeToURL:atomically:encoding:error:`.
//!
//! A file is read as NSData reads one, with its errors (no such file, no
//! permission, …), and its bytes decoded as `-initWithData:encoding:`
//! decodes them, which a subclass's initializer may answer. Bytes the
//! encoding doesn't apply to are NSFileReadInapplicableStringEncodingError,
//! and text it can't hold is NSFileWriteInapplicableStringEncodingError,
//! each naming the file and the encoding. Working out the encoding takes a
//! byte order mark (UTF-8, UTF-16 or UTF-32), or UTF-8 that is valid;
//! anything else is NSFileReadUnknownStringEncodingError.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, NSObject, Sel};
use objc2::{ClassType, msg_send, sel};
use objc2_foundation::{NSData, NSError, NSString, NSStringEncoding, NSURL};

use super::encoding;
use crate::error::code;

/// Where the text comes from or goes: a path or a URL.
enum Place<'a> {
    Path(String),
    Url(&'a NSURL),
}

impl Place<'_> {
    /// The file's bytes, or the file's error stored in `error`.
    ///
    /// # Safety
    ///
    /// `error` is null or points to room for an error.
    unsafe fn read(&self, error: *mut *mut NSError) -> Option<Vec<u8>> {
        // SAFETY: per this function's contract.
        unsafe {
            match self {
                Place::Path(p) => crate::data::read_path(p, error),
                Place::Url(u) => crate::data::read_url(u, error),
            }
        }
    }

    /// Store an error about the text encoding.
    ///
    /// # Safety
    ///
    /// As [`Place::read`].
    unsafe fn encoding_error(&self, code: isize, encoding: u64, error: *mut *mut NSError) {
        let made = match self {
            Place::Path(p) => crate::error::encoding_error(code, Some(p), None, encoding),
            Place::Url(u) => crate::error::encoding_error(code, None, Some(u), encoding),
        };
        // SAFETY: per this function's contract.
        unsafe { crate::error::set(error, made) };
    }
}

/// `[this initWithData:… encoding:…]` with the file's bytes; nil, with
/// `this` released, when the file can't be read or decoded.
fn init_decoding(
    this: *mut AnyObject,
    place: Place<'_>,
    raw: NSStringEncoding,
    error: *mut *mut NSError,
) -> *mut AnyObject {
    // SAFETY: the caller passes null or room for an error.
    let Some(bytes) = (unsafe { place.read(error) }) else {
        // SAFETY: an initializer consumes its receiver; the placeholder's
        // release does nothing.
        unsafe { objc2::ffi::objc_release(this.cast()) };
        return std::ptr::null_mut();
    };
    let data = NSData::with_bytes(&bytes);
    // SAFETY: every string class answers -initWithData:encoding:, which
    // consumes the receiver and returns +1 or nil.
    let made: *mut AnyObject = unsafe { msg_send![this, initWithData: &*data, encoding: raw] };
    if made.is_null() {
        let ns = u64::from(encoding::arg(raw));
        // SAFETY: as above.
        unsafe { place.encoding_error(code::FILE_READ_INAPPLICABLE_STRING_ENCODING, ns, error) };
    }
    made
}

/// The encoding the bytes are in, as `usedEncoding:` works it out: by a
/// byte order mark, or UTF-8 if they are; with how many bytes the mark
/// takes.
fn guess(bytes: &[u8]) -> Option<(u32, usize)> {
    Some(match bytes {
        [0xEF, 0xBB, 0xBF, ..] => (encoding::UTF8, 3),
        [0xFF, 0xFE, 0, 0, ..] | [0, 0, 0xFE, 0xFF, ..] => (encoding::UTF32, 0),
        [0xFF, 0xFE, ..] | [0xFE, 0xFF, ..] => (encoding::UTF16, 0),
        _ if std::str::from_utf8(bytes).is_ok() => (encoding::UTF8, 0),
        _ => return None,
    })
}

/// `[this initWithData:… encoding:…]` in the encoding the file's bytes are
/// in, stored in `used`.
fn init_guessing(
    this: *mut AnyObject,
    place: Place<'_>,
    used: *mut NSStringEncoding,
    error: *mut *mut NSError,
) -> *mut AnyObject {
    // SAFETY: the caller passes null or room for an error.
    let Some(bytes) = (unsafe { place.read(error) }) else {
        // SAFETY: as in `init_decoding`.
        unsafe { objc2::ffi::objc_release(this.cast()) };
        return std::ptr::null_mut();
    };
    let Some((found, mark)) = guess(&bytes) else {
        // SAFETY: as in `init_decoding`.
        unsafe {
            objc2::ffi::objc_release(this.cast());
            place.encoding_error(code::FILE_READ_UNKNOWN_STRING_ENCODING, 0, error);
        }
        return std::ptr::null_mut();
    };
    let data = NSData::with_bytes(&bytes[mark..]);
    let raw = encoding::raw(found);
    // SAFETY: as in `init_decoding`.
    let made: *mut AnyObject = unsafe { msg_send![this, initWithData: &*data, encoding: raw] };
    if made.is_null() {
        // SAFETY: as in `init_decoding`.
        unsafe { place.encoding_error(code::FILE_READ_UNKNOWN_STRING_ENCODING, 0, error) };
    } else if !used.is_null() {
        // SAFETY: the caller passes null or room for an encoding.
        unsafe { used.write(raw) };
    }
    made
}

/// Write the string's text in `raw` to `place`, through a temporary file
/// when `atomically`.
fn write(
    this: &AnyObject,
    place: Place<'_>,
    atomically: bool,
    raw: NSStringEncoding,
    error: *mut *mut NSError,
) -> Bool {
    // SAFETY: -dataUsingEncoding: takes an encoding and returns data or
    // nil when the encoding can't hold the text.
    let data: Option<Retained<NSData>> = unsafe { msg_send![this, dataUsingEncoding: raw] };
    let Some(data) = data else {
        let ns = u64::from(encoding::arg(raw));
        // SAFETY: the caller passes null or room for an error.
        unsafe { place.encoding_error(code::FILE_WRITE_INAPPLICABLE_STRING_ENCODING, ns, error) };
        return Bool::NO;
    };
    let options = if atomically { crate::data::WRITE_ATOMIC } else { 0 };
    // SAFETY: nothing mutates the data while it's written; the caller
    // passes null or room for an error.
    let written = unsafe {
        let bytes = crate::data::bytes(&data);
        match &place {
            Place::Path(p) => crate::data::write_path(bytes, p, options, error),
            Place::Url(u) => crate::data::write_url(bytes, u, options, error),
        }
    };
    Bool::new(written)
}

// The instance methods.

extern "C-unwind" fn init_file(
    this: *mut AnyObject,
    _: Sel,
    path: &NSString,
    raw: NSStringEncoding,
    error: *mut *mut NSError,
) -> *mut AnyObject {
    init_decoding(this, Place::Path(path.to_string()), raw, error)
}

extern "C-unwind" fn init_url(
    this: *mut AnyObject,
    _: Sel,
    url: &NSURL,
    raw: NSStringEncoding,
    error: *mut *mut NSError,
) -> *mut AnyObject {
    init_decoding(this, Place::Url(url), raw, error)
}

extern "C-unwind" fn init_file_used(
    this: *mut AnyObject,
    _: Sel,
    path: &NSString,
    used: *mut NSStringEncoding,
    error: *mut *mut NSError,
) -> *mut AnyObject {
    init_guessing(this, Place::Path(path.to_string()), used, error)
}

extern "C-unwind" fn init_url_used(
    this: *mut AnyObject,
    _: Sel,
    url: &NSURL,
    used: *mut NSStringEncoding,
    error: *mut *mut NSError,
) -> *mut AnyObject {
    init_guessing(this, Place::Url(url), used, error)
}

extern "C-unwind" fn write_file(
    this: &AnyObject,
    _: Sel,
    path: &NSString,
    atomically: Bool,
    raw: NSStringEncoding,
    error: *mut *mut NSError,
) -> Bool {
    write(this, Place::Path(path.to_string()), atomically.as_bool(), raw, error)
}

extern "C-unwind" fn write_url(
    this: &AnyObject,
    _: Sel,
    url: &NSURL,
    atomically: Bool,
    raw: NSStringEncoding,
    error: *mut *mut NSError,
) -> Bool {
    write(this, Place::Url(url), atomically.as_bool(), raw, error)
}

// The class methods: `+alloc` and the initializer, sent to the receiving
// class (an NSMutableString's are mutable).

/// `[[cls alloc] <init>]`, autoreleased.
fn made(cls: &AnyClass, init: impl FnOnce(*mut AnyObject) -> *mut AnyObject) -> *mut AnyObject {
    // SAFETY: every string class answers +alloc.
    let obj: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    // SAFETY: a +1 object or nil, returned autoreleased.
    unsafe { objc2::ffi::objc_autoreleaseReturnValue(init(obj).cast()).cast() }
}

extern "C-unwind" fn string_file(
    cls: &AnyClass,
    _: Sel,
    path: &NSString,
    raw: NSStringEncoding,
    error: *mut *mut NSError,
) -> *mut AnyObject {
    // SAFETY: the initializer this module adds to every string class.
    made(cls, |obj| unsafe { msg_send![obj, initWithContentsOfFile: path, encoding: raw, error: error] })
}

extern "C-unwind" fn string_url(
    cls: &AnyClass,
    _: Sel,
    url: &NSURL,
    raw: NSStringEncoding,
    error: *mut *mut NSError,
) -> *mut AnyObject {
    // SAFETY: as above.
    made(cls, |obj| unsafe { msg_send![obj, initWithContentsOfURL: url, encoding: raw, error: error] })
}

extern "C-unwind" fn string_file_used(
    cls: &AnyClass,
    _: Sel,
    path: &NSString,
    used: *mut NSStringEncoding,
    error: *mut *mut NSError,
) -> *mut AnyObject {
    // SAFETY: as above.
    made(cls, |obj| unsafe { msg_send![obj, initWithContentsOfFile: path, usedEncoding: used, error: error] })
}

extern "C-unwind" fn string_url_used(
    cls: &AnyClass,
    _: Sel,
    url: &NSURL,
    used: *mut NSStringEncoding,
    error: *mut *mut NSError,
) -> *mut AnyObject {
    // SAFETY: as above.
    made(cls, |obj| unsafe { msg_send![obj, initWithContentsOfURL: url, usedEncoding: used, error: error] })
}

/// `+localizedNameOfStringEncoding:`: the encoding's name, or an empty
/// string for one Sidestep doesn't have.
extern "C-unwind" fn localized_name(_: &AnyClass, _: Sel, raw: NSStringEncoding) -> *mut AnyObject {
    let name = crate::cf::string_encoding::ns_name(u64::from(encoding::arg(raw))).unwrap_or("");
    Retained::autorelease_return(NSString::from_str(name)).cast()
}

/// Add the methods to NSString and its metaclass.
pub(crate) fn install(target: &AnyClass) {
    let mut b = ClassBuilder::new(c"_SidestepStringFiles", NSObject::class())
        .expect("sidestep: the string file methods are defined once");
    // SAFETY: each function matches the selector's convention.
    unsafe {
        b.add_method(
            sel!(initWithContentsOfFile:encoding:error:),
            init_file as extern "C-unwind" fn(_, _, _, _, _) -> _,
        );
        b.add_method(sel!(initWithContentsOfURL:encoding:error:), init_url as extern "C-unwind" fn(_, _, _, _, _) -> _);
        b.add_method(
            sel!(initWithContentsOfFile:usedEncoding:error:),
            init_file_used as extern "C-unwind" fn(_, _, _, _, _) -> _,
        );
        b.add_method(
            sel!(initWithContentsOfURL:usedEncoding:error:),
            init_url_used as extern "C-unwind" fn(_, _, _, _, _) -> _,
        );
        b.add_method(
            sel!(writeToFile:atomically:encoding:error:),
            write_file as extern "C-unwind" fn(_, _, _, _, _, _) -> _,
        );
        b.add_method(
            sel!(writeToURL:atomically:encoding:error:),
            write_url as extern "C-unwind" fn(_, _, _, _, _, _) -> _,
        );
    }
    super::install::copy_methods(b.register(), target, false);
    super::install::class_methods(target, c"_SidestepStringFileClassMethods", |b| {
        // SAFETY: as above.
        unsafe {
            b.add_method(
                sel!(stringWithContentsOfFile:encoding:error:),
                string_file as extern "C-unwind" fn(_, _, _, _, _) -> _,
            );
            b.add_method(
                sel!(stringWithContentsOfURL:encoding:error:),
                string_url as extern "C-unwind" fn(_, _, _, _, _) -> _,
            );
            b.add_method(
                sel!(stringWithContentsOfFile:usedEncoding:error:),
                string_file_used as extern "C-unwind" fn(_, _, _, _, _) -> _,
            );
            b.add_method(
                sel!(stringWithContentsOfURL:usedEncoding:error:),
                string_url_used as extern "C-unwind" fn(_, _, _, _, _) -> _,
            );
            b.add_method(sel!(localizedNameOfStringEncoding:), localized_name as extern "C-unwind" fn(_, _, _) -> _);
        }
    });
}
