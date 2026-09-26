//! An Objective-C runtime written in Rust.
//!
//! It exports the C ABI of GNUstep's libobjc2 in the flavor objc2's
//! `gnustep-2-1` feature expects: `objc_msg_lookup` dispatch, the class
//! construction functions, reference counting, weak references, autorelease
//! pools and the blocks runtime. No Objective-C compiler or libobjc is
//! involved.
//!
//! Framework classes (Foundation, AppKit) are static *shells*: objc2 refers
//! to a class through the linker symbol `._OBJC_CLASS_<Name>`, so each such
//! class must exist at link time. [`static_class!`] declares a shell and the
//! function that defines it; the runtime runs that function the first time the
//! class is used. The function normally calls the `class()` method of an
//! objc2 `define_class!` type whose name matches the shell, and
//! `objc_allocateClassPair` hands the shell to it instead of allocating a new
//! class.
//!
//! See `docs/abi.md` in the repository for the contract this crate keeps.
#![cfg(not(target_vendor = "apple"))]
#![allow(non_snake_case, non_upper_case_globals)]

mod arc;
mod associated;
mod blocks;
mod cache;
mod class;
mod encoding;
mod ivar;
mod message;
mod method;
mod nsobject;
mod object;
mod protocol;
mod selector;
mod sync;
mod util;

pub use class::Class;
pub use object::{Object, ObjectRef, StaticObject};
pub use selector::Selector;

pub use nsobject::{NSOBJECT_CLASS, NSOBJECT_METACLASS};

/// A method implementation: the C function a selector resolves to.
pub type Imp = unsafe extern "C-unwind" fn();

/// Objective-C `BOOL`. On GNUstep outside Windows it is an `unsigned char`,
/// which is also what objc2 encodes it as.
pub type Bool = u8;
pub(crate) const YES: Bool = 1;
pub(crate) const NO: Bool = 0;

/// Declare a framework class that objc2 can link against.
///
/// ```ignore
/// sidestep_runtime::static_class!(pub NSSTRING, NSSTRING_META = "NSString", || {
///     let _ = NSStringImpl::class();
/// });
/// ```
///
/// This exports `._OBJC_CLASS_NSString` and `._OBJC_METACLASS_NSString`. The
/// closure runs once, the first time the runtime needs the class. It must
/// register a class with the same name (normally by forcing an objc2
/// `define_class!` type), and it must not call back into the class it is
/// defining.
#[macro_export]
macro_rules! static_class {
    ($vis:vis $class:ident, $meta:ident = $name:literal, $load:expr $(,)?) => {
        #[unsafe(export_name = concat!("._OBJC_CLASS_", $name))]
        $vis static $class: $crate::Class =
            $crate::Class::shell(&$meta, concat!($name, "\0"), $load);
        #[unsafe(export_name = concat!("._OBJC_METACLASS_", $name))]
        $vis static $meta: $crate::Class =
            $crate::Class::meta_shell(&$class, concat!($name, "\0"));
    };
}
