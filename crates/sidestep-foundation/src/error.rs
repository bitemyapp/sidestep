//! `NSError`, its domain and user-info key constants, and the errors
//! Foundation's file operations report.
//!
//! An error is a domain, a code and a user-info dictionary. The localized
//! texts come from the dictionary first (`NSLocalizedDescription`, or
//! `NSLocalizedFailure` followed by the failure reason), then from what
//! the domain says about the code: the Cocoa file errors below, or
//! `strerror` for POSIX errors. Past that, the description is the generic
//! "couldn't be completed" sentence naming the domain and code, as on
//! macOS.
//!
//! File operations report Cocoa codes chosen from the `errno` of the
//! failing call (per operation: a missing file is 260 when reading and 4
//! when writing), with the path, its file URL and the POSIX error as
//! `NSUnderlyingError` in the user info; `conformance/tests/services.rs`
//! pins the mapping against macOS.

use std::fmt::Write as _;
use std::ptr;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSDictionary, NSError, NSInteger, NSString, NSUInteger, NSURL, NSZone};

use crate::runloop::modes::{constant, exported_strings};

sidestep_runtime::static_class!(pub(crate) NSERROR, NSERROR_META = "NSError", || {
    let _ = NSErrorImpl::class();
    crate::perform::install();
});

exported_strings! {
    NSCocoaErrorDomain, pub(crate) COCOA_DOMAIN = "NSCocoaErrorDomain";
    NSPOSIXErrorDomain, pub(crate) POSIX_DOMAIN = "NSPOSIXErrorDomain";
    NSOSStatusErrorDomain, OSSTATUS_DOMAIN = "NSOSStatusErrorDomain";
    NSMachErrorDomain, MACH_DOMAIN = "NSMachErrorDomain";
    NSUnderlyingErrorKey, pub(crate) UNDERLYING = "NSUnderlyingError";
    NSMultipleUnderlyingErrorsKey, MULTIPLE_UNDERLYING = "NSMultipleUnderlyingErrorsKey";
    NSLocalizedDescriptionKey, pub(crate) DESCRIPTION = "NSLocalizedDescription";
    NSLocalizedFailureReasonErrorKey, pub(crate) REASON = "NSLocalizedFailureReason";
    NSLocalizedRecoverySuggestionErrorKey, pub(crate) SUGGESTION = "NSLocalizedRecoverySuggestion";
    NSLocalizedRecoveryOptionsErrorKey, RECOVERY_OPTIONS = "NSLocalizedRecoveryOptions";
    NSRecoveryAttempterErrorKey, RECOVERY_ATTEMPTER = "NSRecoveryAttempter";
    NSHelpAnchorErrorKey, HELP_ANCHOR = "NSHelpAnchor";
    NSDebugDescriptionErrorKey, pub(crate) DEBUG_DESCRIPTION = "NSDebugDescription";
    NSLocalizedFailureErrorKey, pub(crate) FAILURE = "NSLocalizedFailure";
    NSStringEncodingErrorKey, STRING_ENCODING = "NSStringEncoding";
    NSURLErrorKey, pub(crate) URL = "NSURL";
    NSFilePathErrorKey, pub(crate) FILE_PATH = "NSFilePath";
}

/// Cocoa error codes Foundation reports.
pub(crate) mod code {
    pub(crate) const FILE_NO_SUCH_FILE: isize = 4;
    pub(crate) const FILE_READ_UNKNOWN: isize = 256;
    pub(crate) const FILE_READ_NO_PERMISSION: isize = 257;
    pub(crate) const FILE_READ_INVALID_FILE_NAME: isize = 258;
    pub(crate) const FILE_READ_NO_SUCH_FILE: isize = 260;
    pub(crate) const FILE_READ_UNSUPPORTED_SCHEME: isize = 262;
    pub(crate) const FILE_READ_TOO_LARGE: isize = 263;
    pub(crate) const FILE_WRITE_UNKNOWN: isize = 512;
    pub(crate) const FILE_WRITE_NO_PERMISSION: isize = 513;
    pub(crate) const FILE_WRITE_INVALID_FILE_NAME: isize = 514;
    pub(crate) const FILE_WRITE_FILE_EXISTS: isize = 516;
    pub(crate) const FILE_WRITE_UNSUPPORTED_SCHEME: isize = 518;
    pub(crate) const FILE_WRITE_OUT_OF_SPACE: isize = 640;
    pub(crate) const FILE_WRITE_VOLUME_READ_ONLY: isize = 642;
    pub(crate) const PROPERTY_LIST_READ_CORRUPT: isize = 3840;
    pub(crate) const PROPERTY_LIST_WRITE_INVALID: isize = 3851;
}

pub(crate) struct ErrorIvars {
    domain: Retained<NSString>,
    code: NSInteger,
    user_info: Option<Retained<AnyObject>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSError"]
    #[ivars = ErrorIvars]
    pub(crate) struct NSErrorImpl;

    impl NSErrorImpl {
        #[unsafe(method_id(initWithDomain:code:userInfo:))]
        fn init_with_domain(
            this: Allocated<Self>,
            domain: &NSString,
            code: NSInteger,
            user_info: Option<&AnyObject>,
        ) -> Retained<Self> {
            init(this, domain, code, user_info)
        }

        #[unsafe(method_id(errorWithDomain:code:userInfo:))]
        fn error_with_domain(domain: &NSString, code: NSInteger, user_info: Option<&AnyObject>) -> Retained<Self> {
            init(Self::alloc(), domain, code, user_info)
        }

        #[unsafe(method_id(domain))]
        fn domain(&self) -> Retained<NSString> {
            self.ivars().domain.clone()
        }

        #[unsafe(method(code))]
        fn code(&self) -> NSInteger {
            self.ivars().code
        }

        #[unsafe(method_id(userInfo))]
        fn user_info(&self) -> Retained<AnyObject> {
            match &self.ivars().user_info {
                Some(info) => info.clone(),
                None => NSDictionary::<AnyObject, AnyObject>::new().into(),
            }
        }

        #[unsafe(method_id(localizedDescription))]
        fn localized_description(&self) -> Retained<NSString> {
            NSString::from_str(&self.texts().description())
        }

        #[unsafe(method_id(localizedFailureReason))]
        fn localized_failure_reason(&self) -> Option<Retained<NSString>> {
            self.texts().reason().map(|r| NSString::from_str(&r))
        }

        #[unsafe(method_id(localizedRecoverySuggestion))]
        fn localized_recovery_suggestion(&self) -> Option<Retained<NSString>> {
            self.texts().suggestion().map(|r| NSString::from_str(&r))
        }

        #[unsafe(method_id(localizedRecoveryOptions))]
        fn localized_recovery_options(&self) -> Option<Retained<AnyObject>> {
            self.info(&RECOVERY_OPTIONS)
        }

        #[unsafe(method_id(recoveryAttempter))]
        fn recovery_attempter(&self) -> Option<Retained<AnyObject>> {
            self.info(&RECOVERY_ATTEMPTER)
        }

        #[unsafe(method_id(helpAnchor))]
        fn help_anchor(&self) -> Option<Retained<NSString>> {
            self.info_text(&HELP_ANCHOR).map(|t| NSString::from_str(&t))
        }

        #[unsafe(method_id(underlyingErrors))]
        fn underlying_errors(&self) -> Retained<AnyObject> {
            let errors: Vec<Retained<AnyObject>> = self.info(&UNDERLYING).into_iter().collect();
            objc2_foundation::NSArray::from_retained_slice(&errors).into()
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSError>()).is_some_and(|o| self.equals(error_impl(o)))
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            crate::string::hash_str(&self.ivars().domain.to_string()) ^ self.ivars().code as NSUInteger
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&self.description_text())
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            // Immutable.
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSErrorImpl {}
);

/// The texts an error can give, worked out from its user info and domain.
struct Texts<'a> {
    error: &'a NSErrorImpl,
}

impl Texts<'_> {
    fn domain(&self) -> String {
        self.error.ivars().domain.to_string()
    }

    fn cocoa(&self) -> Option<Cocoa> {
        (self.domain() == "NSCocoaErrorDomain").then(|| Cocoa::new(self.error))?
    }

    fn posix_reason(&self) -> Option<String> {
        if self.domain() != "NSPOSIXErrorDomain" {
            return None;
        }
        strerror(i32::try_from(self.error.ivars().code).ok()?)
    }

    fn reason(&self) -> Option<String> {
        self.error.info_text(&REASON).or_else(|| self.cocoa().and_then(|c| c.reason)).or_else(|| self.posix_reason())
    }

    fn suggestion(&self) -> Option<String> {
        self.error.info_text(&SUGGESTION).or_else(|| self.cocoa().and_then(|c| c.suggestion))
    }

    fn description(&self) -> String {
        if let Some(text) = self.error.info_text(&DESCRIPTION) {
            return text;
        }
        if let Some(failure) = self.error.info_text(&FAILURE) {
            return match self.error.info_text(&REASON) {
                Some(reason) => format!("{failure} {reason}"),
                None => failure,
            };
        }
        if let Some(cocoa) = self.cocoa() {
            return cocoa.description;
        }
        if let Some(reason) = self.reason() {
            return format!("The operation couldn\u{2019}t be completed. {reason}");
        }
        let code = self.error.ivars().code;
        match self.domain().as_str() {
            "NSCocoaErrorDomain" => format!("The operation couldn\u{2019}t be completed. (Cocoa error {code}.)"),
            "NSPOSIXErrorDomain" => {
                format!("The operation couldn\u{2019}t be completed. (POSIX error {code} - Unknown error: {code})")
            }
            domain => format!("The operation couldn\u{2019}t be completed. ({domain} error {code}.)"),
        }
    }

    /// What `-description` quotes: a text from the user info or the
    /// domain, never the generic sentence.
    fn quoted(&self) -> Option<String> {
        self.error
            .info_text(&DESCRIPTION)
            .or_else(|| self.error.info_text(&REASON))
            .or_else(|| self.error.info_text(&DEBUG_DESCRIPTION))
            .or_else(|| self.cocoa().map(|c| c.description))
            .or_else(|| self.posix_reason())
    }
}

/// A Cocoa error's texts, naming the file from the user info when there
/// is one.
struct Cocoa {
    description: String,
    reason: Option<String>,
    suggestion: Option<String>,
}

impl Cocoa {
    fn new(error: &NSErrorImpl) -> Option<Cocoa> {
        let path = error.info_text(&FILE_PATH).or_else(|| {
            let url = error.info(&URL)?;
            crate::url::url_impl(url.downcast_ref::<NSURL>()?).path_text()
        });
        let name = path.as_deref().map(|p| {
            let trimmed = p.trim_end_matches('/');
            trimmed.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(p).to_string()
        });
        let folder = path.as_deref().and_then(|p| {
            let parent = std::path::Path::new(p.trim_end_matches('/')).parent()?;
            Some(
                parent
                    .file_name()
                    .map_or_else(|| parent.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned()),
            )
        });
        let scheme = error.info(&URL).and_then(|u| {
            let url = crate::url::url_impl(u.downcast_ref::<NSURL>()?);
            let (s, p) = url.absolute();
            p.scheme.clone().map(|r| s[r].to_string())
        });
        let file = |with: &str, without: &str| match &name {
            Some(name) => with.replace("{}", &format!("\u{201c}{name}\u{201d}")),
            None => without.to_string(),
        };
        let unsupported = "The specified URL type isn\u{2019}t supported.";
        let (description, reason, suggestion): (String, Option<&str>, Option<String>) = match error.ivars().code {
            code::FILE_NO_SUCH_FILE => (
                file("The file {} doesn\u{2019}t exist.", "The file doesn\u{2019}t exist."),
                Some("The file doesn\u{2019}t exist."),
                None,
            ),
            code::FILE_READ_UNKNOWN => {
                (file("The file {} couldn\u{2019}t be opened.", "The file couldn\u{2019}t be opened."), None, None)
            }
            code::FILE_READ_NO_PERMISSION => (
                file(
                    "The file {} couldn\u{2019}t be opened because you don\u{2019}t have permission to view it.",
                    "The file couldn\u{2019}t be opened because you don\u{2019}t have permission to view it.",
                ),
                Some("You don\u{2019}t have permission."),
                None,
            ),
            code::FILE_READ_INVALID_FILE_NAME => (
                file(
                    "The file {} couldn\u{2019}t be opened because the file name is incorrect.",
                    "The file couldn\u{2019}t be opened because the file name is incorrect.",
                ),
                Some("The file name is invalid."),
                None,
            ),
            code::FILE_READ_NO_SUCH_FILE => (
                file(
                    "The file {} couldn\u{2019}t be opened because there is no such file.",
                    "The file couldn\u{2019}t be opened because it doesn\u{2019}t exist.",
                ),
                Some("The file doesn\u{2019}t exist."),
                None,
            ),
            code::FILE_READ_UNSUPPORTED_SCHEME => (
                file(
                    "The file {} couldn\u{2019}t be opened because the specified URL type isn\u{2019}t supported.",
                    "The file couldn\u{2019}t be opened because the specified URL type isn\u{2019}t supported.",
                ),
                Some(unsupported),
                None,
            ),
            code::FILE_READ_TOO_LARGE => (
                file(
                    "The file {} couldn\u{2019}t be opened because it is too large.",
                    "The file couldn\u{2019}t be opened because it is too large.",
                ),
                Some("The file is too large."),
                None,
            ),
            code::FILE_WRITE_UNKNOWN => {
                (file("The file {} couldn\u{2019}t be saved.", "The file couldn\u{2019}t be saved."), None, None)
            }
            code::FILE_WRITE_NO_PERMISSION => (
                file(
                    "You don\u{2019}t have permission to save the file {}.",
                    "You don\u{2019}t have permission to save the file.",
                ),
                Some("You don\u{2019}t have permission."),
                None,
            ),
            code::FILE_WRITE_INVALID_FILE_NAME => (
                file(
                    "The file {} couldn\u{2019}t be saved because the file name is incorrect.",
                    "The file couldn\u{2019}t be saved because the file name is incorrect.",
                ),
                Some("The file name is invalid."),
                None,
            ),
            code::FILE_WRITE_FILE_EXISTS => {
                let description = match (&name, &folder) {
                    (Some(name), Some(folder)) => format!(
                        "The file \u{201c}{name}\u{201d} couldn\u{2019}t be saved in the folder \u{201c}{folder}\u{201d} because a file with the same name already exists."
                    ),
                    _ => file(
                        "The file {} couldn\u{2019}t be saved because a file with the same name already exists.",
                        "The file couldn\u{2019}t be saved because a file with the same name already exists.",
                    ),
                };
                let reason = name.as_ref().map(|n| format!("A file with the name \u{201c}{n}\u{201d} already exists."));
                return Some(Cocoa {
                    description,
                    reason,
                    suggestion: Some(
                        "To save the file, either provide a different name, or move aside or delete the existing file, and try again."
                            .to_string(),
                    ),
                });
            }
            code::FILE_WRITE_UNSUPPORTED_SCHEME => (
                match (&name, &scheme) {
                    (Some(name), Some(scheme)) => format!(
                        "The file \u{201c}{name}\u{201d} couldn\u{2019}t be saved because URL type {scheme} isn\u{2019}t supported."
                    ),
                    _ => file(
                        "The file {} couldn\u{2019}t be saved because the specified URL type isn\u{2019}t supported.",
                        "The file couldn\u{2019}t be saved because the specified URL type isn\u{2019}t supported.",
                    ),
                },
                Some(unsupported),
                None,
            ),
            code::FILE_WRITE_OUT_OF_SPACE => (
                file(
                    "The file {} couldn\u{2019}t be saved because there isn\u{2019}t enough space.",
                    "The file couldn\u{2019}t be saved because there isn\u{2019}t enough space.",
                ),
                Some("There isn\u{2019}t enough space."),
                None,
            ),
            code::FILE_WRITE_VOLUME_READ_ONLY => (
                file(
                    "You can\u{2019}t save the file {} because the volume is read only.",
                    "You can\u{2019}t save the file because the volume is read only.",
                ),
                Some("The volume is read only."),
                Some("Try saving the file to another volume.".to_string()),
            ),
            code::PROPERTY_LIST_READ_CORRUPT => (
                "The data couldn\u{2019}t be read because it isn\u{2019}t in the correct format.".to_string(),
                Some("The data isn\u{2019}t in the correct format."),
                None,
            ),
            code::PROPERTY_LIST_WRITE_INVALID => (
                "The data couldn\u{2019}t be written because it isn\u{2019}t a property list.".to_string(),
                Some("The data isn\u{2019}t a property list."),
                None,
            ),
            _ => return None,
        };
        Some(Cocoa { description, reason: reason.map(str::to_string), suggestion })
    }
}

impl NSErrorImpl {
    fn texts(&self) -> Texts<'_> {
        Texts { error: self }
    }

    /// A user-info value.
    fn info(&self, key: &'static crate::ConstantString) -> Option<Retained<AnyObject>> {
        let info = self.ivars().user_info.as_ref()?;
        // SAFETY: the user info is a dictionary; -objectForKey: returns a
        // borrowed value or nil.
        unsafe { msg_send![&**info, objectForKey: constant(key)] }
    }

    /// A user-info value that is a string.
    fn info_text(&self, key: &'static crate::ConstantString) -> Option<String> {
        self.info(key)?.downcast_ref::<NSString>().map(|s| s.to_string())
    }

    fn equals(&self, other: &NSErrorImpl) -> bool {
        let (a, b) = (self.ivars(), other.ivars());
        if a.code != b.code || a.domain.to_string() != b.domain.to_string() {
            return false;
        }
        let empty = |i: &Option<Retained<AnyObject>>| i.as_ref().is_none_or(|i| dictionary_count(i) == 0);
        match (&a.user_info, &b.user_info) {
            (Some(x), Some(y)) => dictionaries_equal(x, y),
            _ => empty(&a.user_info) && empty(&b.user_info),
        }
    }

    fn description_text(&self) -> String {
        let ivars = self.ivars();
        let quoted = self.texts().quoted().unwrap_or_else(|| "(null)".to_string());
        let mut text = format!("Error Domain={} Code={} \"{quoted}\"", ivars.domain, ivars.code);
        if let Some(info) = &ivars.user_info {
            let mut entries = dictionary_entries(info);
            if !entries.is_empty() {
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                text.push_str(" UserInfo={");
                for (i, (key, value)) in entries.iter().enumerate() {
                    if i > 0 {
                        text.push_str(", ");
                    }
                    let _ = write!(text, "{key}={value}");
                }
                text.push('}');
            }
        }
        text
    }
}

fn dictionary_count(dictionary: &AnyObject) -> NSUInteger {
    // SAFETY: -count takes nothing.
    unsafe { msg_send![dictionary, count] }
}

/// Whether two dictionaries hold equal values under equal keys.
fn dictionaries_equal(a: &AnyObject, b: &AnyObject) -> bool {
    if ptr::eq(a, b) {
        return true;
    }
    let count = dictionary_count(a);
    if count != dictionary_count(b) {
        return false;
    }
    let (keys, values) = dictionary_objects(a);
    keys.iter().zip(&values).all(|(&key, &value)| {
        // SAFETY: -objectForKey: returns a borrowed value or nil; the
        // pointers are the dictionary's own, kept alive by it.
        let other: *mut AnyObject = unsafe { msg_send![b, objectForKey: key] };
        !other.is_null() && unsafe { msg_send![&*value, isEqual: other] }
    })
}

/// A dictionary's keys and values, in its order.
fn dictionary_objects(dictionary: &AnyObject) -> (Vec<*mut AnyObject>, Vec<*mut AnyObject>) {
    let count = dictionary_count(dictionary);
    let mut keys: Vec<*mut AnyObject> = vec![ptr::null_mut(); count];
    let mut values: Vec<*mut AnyObject> = vec![ptr::null_mut(); count];
    // SAFETY: both buffers have room for `count` objects.
    let () =
        unsafe { msg_send![dictionary, getObjects: values.as_mut_ptr(), andKeys: keys.as_mut_ptr(), count: count] };
    (keys, values)
}

/// A dictionary's keys and values, described.
fn dictionary_entries(dictionary: &AnyObject) -> Vec<(String, String)> {
    let (keys, values) = dictionary_objects(dictionary);
    keys.iter()
        .zip(&values)
        .filter_map(|(&key, &value)| {
            // SAFETY: the dictionary keeps its keys and values alive.
            let (key, value) = unsafe { (key.as_ref()?, value.as_ref()?) };
            Some((describe(key), describe_value(value)))
        })
        .collect()
}

fn describe(object: &AnyObject) -> String {
    if let Some(text) = object.downcast_ref::<NSString>() {
        return text.to_string();
    }
    // SAFETY: -description returns a string.
    let text: Retained<NSString> = unsafe { msg_send![object, description] };
    text.to_string()
}

fn describe_value(object: &AnyObject) -> String {
    match object.downcast_ref::<NSError>() {
        Some(error) => format!("{:p} {{{}}}", error, error_impl(error).description_text()),
        None => describe(object),
    }
}

fn error_impl(error: &NSError) -> &NSErrorImpl {
    // SAFETY: every NSError is an instance of this class.
    unsafe { &*(error as *const NSError).cast::<NSErrorImpl>() }
}

fn init(
    this: Allocated<NSErrorImpl>,
    domain: &NSString,
    code: NSInteger,
    user_info: Option<&AnyObject>,
) -> Retained<NSErrorImpl> {
    let user_info = user_info.map(|info| {
        // SAFETY: -copy on a dictionary gives an immutable one.
        unsafe { msg_send![info, copy] }
    });
    let this = this.set_ivars(ErrorIvars { domain: NSString::from_str(&domain.to_string()), code, user_info });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// The C library's text for an `errno`, if it knows the number.
pub(crate) fn strerror(errno: i32) -> Option<String> {
    let mut buffer = [0 as libc::c_char; 256];
    // SAFETY: the buffer has room for the text and its NUL.
    let status = unsafe { libc::strerror_r(errno, buffer.as_mut_ptr(), buffer.len()) };
    if status != 0 {
        return None;
    }
    // SAFETY: strerror_r wrote a NUL-terminated string.
    let text = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy().into_owned();
    (!text.starts_with("Unknown error")).then_some(text)
}

/// A new error with these user-info entries.
pub(crate) fn make(
    domain: &NSString,
    code: isize,
    info: &[(&'static crate::ConstantString, Retained<AnyObject>)],
) -> Retained<NSError> {
    let user_info: Option<Retained<NSDictionary<NSString, AnyObject>>> = (!info.is_empty()).then(|| {
        let keys: Vec<&NSString> = info.iter().map(|(k, _)| constant(k)).collect();
        let values: Vec<Retained<AnyObject>> = info.iter().map(|(_, v)| v.clone()).collect();
        NSDictionary::from_retained_objects(&keys, &values)
    });
    // SAFETY: +alloc on the class NSError names, then its initializer.
    unsafe {
        let this: Allocated<NSError> = msg_send![NSError::class(), alloc];
        msg_send![this, initWithDomain: domain, code: code, userInfo: user_info.as_deref()]
    }
}

/// `NSPOSIXErrorDomain` and an `errno`.
pub(crate) fn posix(errno: i32) -> Retained<NSError> {
    make(constant(&POSIX_DOMAIN), errno as isize, &[])
}

/// A Cocoa error with these user-info entries.
pub(crate) fn cocoa(code: isize, info: &[(&'static crate::ConstantString, Retained<AnyObject>)]) -> Retained<NSError> {
    make(constant(&COCOA_DOMAIN), code, info)
}

/// What a file operation was doing when it failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FileOp {
    Read,
    Write,
}

/// The Cocoa code for a failed file operation's `errno`.
pub(crate) fn file_code(op: FileOp, errno: i32) -> isize {
    match (op, errno) {
        (FileOp::Read, libc::ENOENT) => code::FILE_READ_NO_SUCH_FILE,
        (FileOp::Read, libc::EACCES | libc::EPERM) => code::FILE_READ_NO_PERMISSION,
        (FileOp::Read, libc::ENAMETOOLONG) => code::FILE_READ_INVALID_FILE_NAME,
        (FileOp::Read, libc::EFBIG) => code::FILE_READ_TOO_LARGE,
        (FileOp::Read, _) => code::FILE_READ_UNKNOWN,
        (FileOp::Write, libc::ENOENT) => code::FILE_NO_SUCH_FILE,
        (FileOp::Write, libc::EACCES | libc::EPERM) => code::FILE_WRITE_NO_PERMISSION,
        (FileOp::Write, libc::ENAMETOOLONG) => code::FILE_WRITE_INVALID_FILE_NAME,
        (FileOp::Write, libc::EEXIST) => code::FILE_WRITE_FILE_EXISTS,
        (FileOp::Write, libc::ENOSPC | libc::EDQUOT) => code::FILE_WRITE_OUT_OF_SPACE,
        (FileOp::Write, libc::EROFS) => code::FILE_WRITE_VOLUME_READ_ONLY,
        (FileOp::Write, _) => code::FILE_WRITE_UNKNOWN,
    }
}

/// The error for a file operation on `path` that failed with `error`: a
/// Cocoa code from its `errno`, the path and its URL, and the POSIX error
/// underneath.
pub(crate) fn file(op: FileOp, error: &std::io::Error, path: &str) -> Retained<NSError> {
    let errno = error.raw_os_error().unwrap_or(libc::EIO);
    file_with_code(file_code(op, errno), Some(errno), path)
}

/// A file error with a given code, and the POSIX error underneath if any.
pub(crate) fn file_with_code(code: isize, errno: Option<i32>, path: &str) -> Retained<NSError> {
    let mut info: Vec<(&'static crate::ConstantString, Retained<AnyObject>)> =
        vec![(&FILE_PATH, NSString::from_str(path).into())];
    if let Some(url) = crate::url::file_url(std::path::Path::new(path)) {
        info.push((&URL, url.into()));
    }
    if let Some(errno) = errno {
        info.push((&UNDERLYING, posix(errno).into()));
    }
    cocoa(code, &info)
}

/// Store an error in a caller's `NSError **`, autoreleased, if it passed
/// one.
///
/// # Safety
///
/// `out` is null or points to writable storage for an object pointer.
pub(crate) unsafe fn set(out: *mut *mut NSError, error: Retained<NSError>) {
    if !out.is_null() {
        // SAFETY: the caller's storage, per this function's contract.
        unsafe { out.write(Retained::autorelease_ptr(error)) };
    }
}
