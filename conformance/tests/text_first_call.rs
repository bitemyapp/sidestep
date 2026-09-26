//! Measuring text as a process's first AppKit call, with no attributes, so
//! that no AppKit class has been used before. Each file in tests/ is its
//! own process.
//!
//! On macOS, `NSString` has its drawing methods from the start; Sidestep
//! adds them with a link-time category, attached when `NSString` registers,
//! before any of AppKit's own classes has loaded.

use objc2_app_kit::NSStringDrawing;
use objc2_foundation::NSString;

use sidestep as _;

#[test]
fn measuring_is_the_first_appkit_call() {
    // SAFETY: no attributes.
    let size = unsafe { NSString::from_str("Hello").sizeWithAttributes(None) };
    assert!(size.width > 0.0 && size.height > 0.0);
}
