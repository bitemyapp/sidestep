//! `NSString`: an immutable string stored as UTF-8, with the UTF-16 view
//! Foundation's API is defined in terms of.

use std::ffi::{c_char, c_void};
use std::hash::{DefaultHasher, Hash, Hasher};

use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSString, NSUInteger, NSZone};

/// Foundation's `NSStringEncoding` values Sidestep understands.
mod encoding {
    pub const ASCII: u32 = 1;
    pub const UTF8: u32 = 4;
    pub const ISO_LATIN1: u32 = 5;
    pub const UTF16: u32 = 10;
    pub const UTF16_BE: u32 = 0x9000_0100;
    pub const UTF16_LE: u32 = 0x9400_0100;
}

pub(crate) struct StringIvars {
    /// The contents followed by a NUL, so `UTF8String` can point into it.
    utf8_nul: Box<[u8]>,
    utf16_len: usize,
}

impl StringIvars {
    fn new(s: String) -> Self {
        let utf16_len = s.encode_utf16().count();
        let mut bytes = s.into_bytes();
        bytes.push(0);
        StringIvars { utf8_nul: bytes.into_boxed_slice(), utf16_len }
    }

    fn as_str(&self) -> &str {
        let bytes = &self.utf8_nul[..self.utf8_nul.len() - 1];
        // SAFETY: built from a String.
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }
}

/// Decode `bytes` from a Foundation string encoding.
fn decode(bytes: &[u8], encoding: u32) -> Option<String> {
    match encoding {
        encoding::UTF8 => String::from_utf8(bytes.to_vec()).ok(),
        encoding::ASCII => bytes.is_ascii().then(|| String::from_utf8(bytes.to_vec()).unwrap()),
        encoding::ISO_LATIN1 => Some(bytes.iter().map(|&b| char::from(b)).collect()),
        encoding::UTF16 | encoding::UTF16_BE | encoding::UTF16_LE => {
            if bytes.len() % 2 != 0 {
                return None;
            }
            let (mut bytes, mut big_endian) = (bytes, encoding == encoding::UTF16_BE);
            if encoding == encoding::UTF16 {
                // A byte order mark decides; without one, host order.
                match bytes {
                    [0xFE, 0xFF, rest @ ..] => (bytes, big_endian) = (rest, true),
                    [0xFF, 0xFE, rest @ ..] => (bytes, big_endian) = (rest, false),
                    _ => big_endian = cfg!(target_endian = "big"),
                }
            }
            let units = bytes.chunks_exact(2).map(|c| {
                let pair = [c[0], c[1]];
                if big_endian { u16::from_be_bytes(pair) } else { u16::from_le_bytes(pair) }
            });
            char::decode_utf16(units).collect::<Result<String, _>>().ok()
        }
        _ => None,
    }
}

/// objc2 passes `NSStringEncoding` as `i32` on GNUstep and as `usize` in
/// objc2-foundation's generated bindings; only the low 32 bits are defined in
/// both cases.
fn encoding_arg(raw: i32) -> u32 {
    raw as u32
}

/// The hash every string class shares, so equal strings hash equally.
pub(crate) fn hash_str(s: &str) -> NSUInteger {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish() as NSUInteger
}

/// `-lengthOfBytesUsingEncoding:`: 0 when the text can't be encoded.
pub(crate) fn byte_length(s: &str, encoding: u32) -> NSUInteger {
    match encoding {
        encoding::UTF8 => s.len(),
        encoding::ASCII if s.is_ascii() => s.len(),
        encoding::ISO_LATIN1 if s.chars().all(|c| (c as u32) < 0x100) => s.chars().count(),
        encoding::UTF16 | encoding::UTF16_BE | encoding::UTF16_LE => 2 * s.encode_utf16().count(),
        _ => 0,
    }
}

pub(crate) fn equals(this: &str, other: &NSString) -> bool {
    autoreleasepool(|pool| {
        // SAFETY: `other` is an NSString and the slice stays inside the pool.
        unsafe { other.to_str(pool) == this }
    })
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSString"]
    #[ivars = StringIvars]
    pub(crate) struct NSStringImpl;

    impl NSStringImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(StringIvars::new(String::new()));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithBytes:length:encoding:))]
        fn init_with_bytes(
            this: Allocated<Self>,
            bytes: *const c_void,
            length: NSUInteger,
            encoding: i32,
        ) -> Option<Retained<Self>> {
            let bytes = if length == 0 {
                &[][..]
            } else {
                // SAFETY: the caller passes `length` readable bytes.
                unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), length) }
            };
            // define_class! rewrites the tail expression's type, so failure
            // is a match arm rather than an early return.
            match decode(bytes, encoding_arg(encoding)) {
                Some(string) => {
                    let this = this.set_ivars(StringIvars::new(string));
                    // SAFETY: as above.
                    unsafe { msg_send![super(this), init] }
                }
                None => None,
            }
        }

        #[unsafe(method(length))]
        fn length(&self) -> NSUInteger {
            self.ivars().utf16_len
        }

        #[unsafe(method(lengthOfBytesUsingEncoding:))]
        fn length_of_bytes_using_encoding(&self, encoding: i32) -> NSUInteger {
            byte_length(self.ivars().as_str(), encoding_arg(encoding))
        }

        #[unsafe(method(UTF8String))]
        fn utf8_string(&self) -> *const c_char {
            self.ivars().utf8_nul.as_ptr().cast()
        }

        #[unsafe(method(characterAtIndex:))]
        fn character_at_index(&self, index: NSUInteger) -> u16 {
            match self.ivars().as_str().encode_utf16().nth(index) {
                Some(unit) => unit,
                None => panic!(
                    "-[NSString characterAtIndex:]: index {index} out of bounds; string length {}",
                    self.ivars().utf16_len
                ),
            }
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            hash_str(self.ivars().as_str())
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            match other.and_then(|o| o.downcast_ref::<NSString>()) {
                Some(other) => equals(self.ivars().as_str(), other),
                None => false,
            }
        }

        #[unsafe(method(isEqualToString:))]
        fn is_equal_to_string(&self, other: &NSString) -> bool {
            equals(self.ivars().as_str(), other)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            // Immutable: a copy is the same object.
            self.retain()
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<Self> {
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSStringImpl {}
);
