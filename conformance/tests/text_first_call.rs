//! Measuring text as a process's first AppKit call, with no attributes, so
//! that no AppKit class has been used before. Each file in tests/ is its
//! own process.
//!
//! On macOS, `NSString` has its drawing methods from the start. Sidestep
//! adds them when an AppKit class loads (`NSResponder`, which the
//! application, windows and views load, `NSColor`, `NSFont` or
//! `NSParagraphStyle`); before that, nothing of AppKit has run. The
//! runtime's link-time categories are what will add them from the start;
//! until it has them, this is ignored on Linux.

use objc2_app_kit::NSStringDrawing;
use objc2_foundation::NSString;

use sidestep as _;

#[test]
#[cfg_attr(not(target_vendor = "apple"), ignore = "needs link-time categories from the runtime")]
fn measuring_is_the_first_appkit_call() {
    // SAFETY: no attributes.
    let size = unsafe { NSString::from_str("Hello").sizeWithAttributes(None) };
    assert!(size.width > 0.0 && size.height > 0.0);
}
