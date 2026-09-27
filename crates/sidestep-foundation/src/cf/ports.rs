//! Mach ports, message ports and file security objects.
//!
//! Linux has no Mach ports and no bootstrap server to register a message
//! port's name with: making either fails as it does on macOS when the
//! kernel or the bootstrap server refuses (NULL), and a remote port is
//! found for no name, as on macOS for a name nobody registered. The rest
//! of their functions take ports that can't exist here, and do nothing.
//!
//! A `CFFileSecurity` holds a file's owner, group, mode and access control
//! list; none of the functions that set them are declared for Linux, so a
//! new one holds none, as on macOS, and clearing any of them succeeds.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, define_class, msg_send};
use objc2_foundation::NSString;

use super::types::{CFTypeID, id, owned};

type Boolean = u8;
type CFIndex = isize;

// Mach ports.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMachPortGetTypeID() -> CFTypeID {
    id::MACH_PORT
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMachPortCreate(
    _allocator: *const c_void,
    _callout: *const c_void,
    _context: *mut c_void,
    should_free_info: *mut Boolean,
) -> *mut c_void {
    if !should_free_info.is_null() {
        // SAFETY: the caller's out-parameter; the context's info is the
        // caller's to free, as no port holds it.
        unsafe { should_free_info.write(1) };
    }
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMachPortCreateRunLoopSource(
    _allocator: *const c_void,
    _port: *const c_void,
    _order: CFIndex,
) -> *mut c_void {
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMachPortGetContext(_port: *const c_void, _context: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMachPortGetInvalidationCallBack(_port: *const c_void) -> *const c_void {
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMachPortSetInvalidationCallBack(_port: *const c_void, _callout: *const c_void) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMachPortInvalidate(_port: *const c_void) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMachPortIsValid(_port: *const c_void) -> Boolean {
    0
}

// Message ports.

/// `kCFMessagePortIsInvalid`.
const MESSAGE_PORT_IS_INVALID: i32 = -3;

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortGetTypeID() -> CFTypeID {
    id::MESSAGE_PORT
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortCreateLocal(
    _allocator: *const c_void,
    _name: *const c_void,
    _callout: *const c_void,
    _context: *mut c_void,
    should_free_info: *mut Boolean,
) -> *mut c_void {
    if !should_free_info.is_null() {
        // SAFETY: the caller's out-parameter, as for a Mach port.
        unsafe { should_free_info.write(1) };
    }
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortCreateRemote(_allocator: *const c_void, _name: *const c_void) -> *mut c_void {
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortCreateRunLoopSource(
    _allocator: *const c_void,
    _local: *const c_void,
    _order: CFIndex,
) -> *mut c_void {
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortGetContext(_port: *const c_void, _context: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortGetInvalidationCallBack(_port: *const c_void) -> *const c_void {
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortSetInvalidationCallBack(_port: *const c_void, _callout: *const c_void) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortGetName(_port: *const c_void) -> *const c_void {
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortSetName(_port: *const c_void, _name: *const c_void) -> Boolean {
    0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortInvalidate(_port: *const c_void) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortIsValid(_port: *const c_void) -> Boolean {
    0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortIsRemote(_port: *const c_void) -> Boolean {
    0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFMessagePortSendRequest(
    _remote: *const c_void,
    _msgid: i32,
    _data: *const c_void,
    _send_timeout: f64,
    _receive_timeout: f64,
    _reply_mode: *const c_void,
    _return_data: *mut *const c_void,
) -> i32 {
    MESSAGE_PORT_IS_INVALID
}

// File security.

define_class!(
    /// A `CFFileSecurity`: no owner, group, mode or access control list.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCFFileSecurity"]
    struct FileSecurity;

    impl FileSecurity {
        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|o| o.downcast_ref::<FileSecurity>().is_some())
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            0
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&format!(
                "<FileSecurity {:p}> {{FILESEC_OWNER = (null), FILESEC_GROUP = (null), FILESEC_MODE = (null), \
                 FILESEC_UUID = (null), FILESEC_GRPUUID = (null), FILESEC_ACL = (null)}}",
                self
            ))
        }
    }
);

/// A new, empty `CFFileSecurity` (a file URL's `kCFURLFileSecurityKey`
/// value too).
pub(crate) fn new_file_security() -> Retained<AnyObject> {
    // SAFETY: NSObject's initializer.
    let this: Retained<FileSecurity> = unsafe { msg_send![FileSecurity::alloc(), init] };
    // SAFETY: every object is an AnyObject.
    unsafe { Retained::cast_unchecked(this) }
}

fn file_security() -> *mut c_void {
    owned(new_file_security())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFFileSecurityGetTypeID() -> CFTypeID {
    id::FILE_SECURITY
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFFileSecurityCreate(_allocator: *const c_void) -> *mut c_void {
    file_security()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFFileSecurityCreateCopy(
    _allocator: *const c_void,
    file_security_: *const c_void,
) -> *mut c_void {
    if file_security_.is_null() {
        return std::ptr::null_mut();
    }
    file_security()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFFileSecurityClearProperties(_file_security: *const c_void, _mask: usize) -> Boolean {
    1
}
