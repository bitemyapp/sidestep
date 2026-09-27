//! CoreFoundation's C functions, as far as objc2-core-foundation's
//! bindings reach them. CoreFoundation objects here are Foundation's own
//! objects (toll-free, as on macOS): an `NSTimer` is a `CFRunLoopTimerRef`,
//! a string a `CFStringRef`. So retain and release are the runtime's, and
//! each function forwards to the Foundation code behind it.
//!
//! objc2-core-foundation links no library on Linux; defining the symbols
//! is all it takes. Functions take `*const c_void`-like raw pointers where
//! the bindings pass `Option<&T>`, which has the same representation.

mod attributed;
mod base;
mod bundle;
mod calendar;
mod charset;
mod collections;
mod data;
pub(crate) mod locale;
pub(crate) mod locale_data;
mod locale_exemplars;
mod ports;
mod preferences;
mod runloop;
mod set;
mod stream;
pub(crate) mod string;
mod string_edit;
mod string_encoding;
mod string_transform;
pub(crate) mod types;
mod url;
mod url_parts;
mod url_resources;
mod value_array;

pub(crate) use ports::new_file_security as file_security;
