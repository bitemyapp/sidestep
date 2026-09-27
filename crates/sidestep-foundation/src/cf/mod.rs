//! CoreFoundation's C functions, as far as objc2-core-foundation's
//! bindings reach them. CoreFoundation objects here are Foundation's own
//! objects (toll-free, as on macOS): an `NSTimer` is a `CFRunLoopTimerRef`,
//! a string a `CFStringRef`. So retain and release are the runtime's, and
//! each function forwards to the Foundation code behind it.
//!
//! objc2-core-foundation links no library on Linux; defining the symbols
//! is all it takes. Functions take `*const c_void`-like raw pointers where
//! the bindings pass `Option<&T>`, which has the same representation.

mod attributed_string;
mod base;
mod collections;
mod data;
mod preferences;
mod runloop;
pub(crate) mod string;
pub(crate) mod types;
mod url;
