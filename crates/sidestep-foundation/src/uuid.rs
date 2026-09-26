//! `NSUUID`: sixteen bytes, random ones (version 4) from `getrandom`.
//!
//! Strings are the usual 8-4-4-4-12 hexadecimal groups, upper case out and
//! either case in; anything else (no dashes, braces) doesn't parse, as on
//! macOS. UUIDs compare byte by byte.

use std::cmp::Ordering;
use std::fmt::Write as _;

use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSComparisonResult, NSString, NSUInteger, NSUUID, NSZone};

sidestep_runtime::static_class!(pub(crate) NSUUID_CLASS, NSUUID_META = "NSUUID", || {
    let _ = NSUUIDImpl::class();
    crate::perform::install();
});

/// A `uuid_t`, encoded as the bindings expect: `[16C]`.
#[repr(transparent)]
pub(crate) struct UuidBytes([u8; 16]);

// SAFETY: sixteen bytes, as the encoding says.
unsafe impl RefEncode for UuidBytes {
    const ENCODING_REF: Encoding = Encoding::Array(16, &u8::ENCODING);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSUUID"]
    #[ivars = [u8; 16]]
    pub(crate) struct NSUUIDImpl;

    impl NSUUIDImpl {
        #[unsafe(method_id(UUID))]
        fn uuid() -> Retained<Self> {
            init_bytes(Self::alloc(), random())
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_bytes(this, random())
        }

        #[unsafe(method_id(initWithUUIDString:))]
        fn init_with_string(this: Allocated<Self>, string: &NSString) -> Option<Retained<Self>> {
            match parse(&string.to_string()) {
                Some(bytes) => Some(init_bytes(this, bytes)),
                None => {
                    drop(this);
                    None
                }
            }
        }

        #[unsafe(method_id(initWithUUIDBytes:))]
        fn init_with_bytes(this: Allocated<Self>, bytes: &UuidBytes) -> Retained<Self> {
            init_bytes(this, bytes.0)
        }

        #[unsafe(method(getUUIDBytes:))]
        fn get_bytes(&self, bytes: &mut UuidBytes) {
            bytes.0 = *self.ivars();
        }

        #[unsafe(method_id(UUIDString))]
        fn uuid_string(&self) -> Retained<NSString> {
            NSString::from_str(&format(self.ivars()))
        }

        #[unsafe(method(compare:))]
        fn compare(&self, other: &NSUUID) -> NSComparisonResult {
            match self.ivars().cmp(uuid_impl(other).ivars()) {
                Ordering::Less => NSComparisonResult::Ascending,
                Ordering::Equal => NSComparisonResult::Same,
                Ordering::Greater => NSComparisonResult::Descending,
            }
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSUUID>()).is_some_and(|o| uuid_impl(o).ivars() == self.ivars())
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            crate::string::hash_bytes(self.ivars())
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&format(self.ivars()))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSUUIDImpl {}
);

fn uuid_impl(uuid: &NSUUID) -> &NSUUIDImpl {
    // SAFETY: every NSUUID is an instance of this class.
    unsafe { &*(uuid as *const NSUUID).cast::<NSUUIDImpl>() }
}

fn init_bytes(this: Allocated<NSUUIDImpl>, bytes: [u8; 16]) -> Retained<NSUUIDImpl> {
    let this = this.set_ivars(bytes);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A random, version 4 UUID.
pub(crate) fn random() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    let mut filled = 0;
    while filled < bytes.len() {
        // SAFETY: the rest of the buffer is writable.
        let n = unsafe { libc::getrandom(bytes[filled..].as_mut_ptr().cast(), bytes.len() - filled, 0) };
        if n > 0 {
            filled += n as usize;
        } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            panic!("getrandom failed: {}", std::io::Error::last_os_error());
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes
}

/// `E621E1F8-C36C-495A-93FC-0C247A3E6E5F`.
pub(crate) fn format(bytes: &[u8; 16]) -> String {
    let mut text = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        let _ = write!(text, "{b:02X}");
    }
    text
}

fn parse(text: &str) -> Option<[u8; 16]> {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return None;
    }
    let mut out = [0u8; 16];
    let mut digits = bytes.iter().enumerate().filter_map(|(i, &c)| {
        if matches!(i, 8 | 13 | 18 | 23) { (c != b'-').then_some(None) } else { Some((c as char).to_digit(16)) }
    });
    for byte in &mut out {
        let high = digits.next()??;
        let low = digits.next()??;
        *byte = (high << 4 | low) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings() {
        let bytes = parse("e621e1f8-c36c-495a-93fc-0c247a3e6e5f").unwrap();
        assert_eq!(format(&bytes), "E621E1F8-C36C-495A-93FC-0C247A3E6E5F");
        assert!(parse("E621E1F8C36C495A93FC0C247A3E6E5F").is_none());
        assert!(parse("{e621e1f8-c36c-495a-93fc-0c247a3e6e5f}").is_none());
        assert!(parse("e621e1f8-c36c-495a-93fc-0c247a3e6e5g").is_none());
        assert!(parse("e621e1f8xc36c-495a-93fc-0c247a3e6e5f").is_none());
        let random = random();
        assert_eq!(random[6] >> 4, 4);
        assert_eq!(random[8] >> 6, 2);
    }
}
