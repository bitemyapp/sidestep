//! AppKit for Sidestep: the classes objc2-app-kit refers to, written in Rust
//! and linked in under their AppKit names.
//!
//! Two threads share the work. The main thread runs AppKit as programs
//! expect: events, timers, the responder chain, views and `drawRect:`.
//! Drawing doesn't touch pixels; it records operations for the parts of a
//! window that changed. A render thread owns the Wayland connection,
//! rasterizes those operations into per-layer caches and presents them
//! (see `backend`). Scroll views get a layer of their own, cut into tiles
//! that the compositor moves when the view scrolls.
//!
//! As in `sidestep-foundation`, each class is a static shell whose loader
//! forces the `define_class!` type, and code here reaches other classes
//! through their objc2-app-kit types.
#![cfg(not(target_vendor = "apple"))]
// `define_class!` recurses once per method, and NSWindow has many.
#![recursion_limit = "256"]

use objc2::ClassType;

mod app;
mod backend;
mod clipboard;
mod cursor;
mod event;
mod graphics;
mod keybindings;
mod pasteboard;
mod protocol;
mod raster;
mod text;
mod views;
mod window;

sidestep_runtime::static_class!(pub NSRESPONDER, NSRESPONDER_META = "NSResponder", || {
    let _ = views::NSResponderImpl::class();
});

sidestep_runtime::static_class!(pub NSVIEW, NSVIEW_META = "NSView", || {
    let _ = views::NSViewImpl::class();
});

sidestep_runtime::static_class!(pub NSCLIPVIEW, NSCLIPVIEW_META = "NSClipView", || {
    let _ = views::NSClipViewImpl::class();
});

sidestep_runtime::static_class!(pub NSSCROLLVIEW, NSSCROLLVIEW_META = "NSScrollView", || {
    let _ = views::NSScrollViewImpl::class();
});

sidestep_runtime::static_class!(pub NSWINDOW, NSWINDOW_META = "NSWindow", || {
    let _ = window::NSWindowImpl::class();
});

sidestep_runtime::static_class!(pub NSAPPLICATION, NSAPPLICATION_META = "NSApplication", || {
    let _ = app::NSApplicationImpl::class();
});

sidestep_runtime::static_class!(pub NSEVENT, NSEVENT_META = "NSEvent", || {
    let _ = event::NSEventImpl::class();
});

sidestep_runtime::static_class!(pub NSCOLOR, NSCOLOR_META = "NSColor", || {
    let _ = graphics::NSColorImpl::class();
});

sidestep_runtime::static_class!(pub NSFONT, NSFONT_META = "NSFont", || {
    let _ = graphics::NSFontImpl::class();
});

sidestep_runtime::static_class!(pub NSBEZIERPATH, NSBEZIERPATH_META = "NSBezierPath", || {
    let _ = graphics::NSBezierPathImpl::class();
});

sidestep_runtime::static_class!(pub NSPASTEBOARD, NSPASTEBOARD_META = "NSPasteboard", || {
    let _ = pasteboard::NSPasteboardImpl::class();
});

sidestep_runtime::static_class!(pub NSCURSOR, NSCURSOR_META = "NSCursor", || {
    let _ = cursor::NSCursorImpl::class();
});

// Pasteboard types and names.
sidestep_foundation::constant_string!(NSPasteboardTypeString = "public.utf8-plain-text");
sidestep_foundation::constant_string!(NSPasteboardTypePDF = "com.adobe.pdf");
sidestep_foundation::constant_string!(NSPasteboardTypeTIFF = "public.tiff");
sidestep_foundation::constant_string!(NSPasteboardTypePNG = "public.png");
sidestep_foundation::constant_string!(NSPasteboardTypeRTF = "public.rtf");
sidestep_foundation::constant_string!(NSPasteboardTypeRTFD = "com.apple.flat-rtfd");
sidestep_foundation::constant_string!(NSPasteboardTypeHTML = "public.html");
sidestep_foundation::constant_string!(NSPasteboardTypeTabularText = "public.utf8-tab-separated-values-text");
sidestep_foundation::constant_string!(NSPasteboardTypeURL = "public.url");
sidestep_foundation::constant_string!(NSPasteboardTypeFileURL = "public.file-url");
sidestep_foundation::constant_string!(NSPasteboardNameGeneral = "Apple CFPasteboard general");
sidestep_foundation::constant_string!(NSPasteboardNameFind = "Apple CFPasteboard find");

// Attribute names for attributed strings and string drawing.
sidestep_foundation::constant_string!(NSFontAttributeName = "NSFont");
sidestep_foundation::constant_string!(NSForegroundColorAttributeName = "NSColor");
sidestep_foundation::constant_string!(NSBackgroundColorAttributeName = "NSBackgroundColor");

// Font weights, as `NSFontWeight` values.
#[unsafe(no_mangle)]
pub static NSFontWeightUltraLight: f64 = -0.8;
#[unsafe(no_mangle)]
pub static NSFontWeightThin: f64 = -0.6;
#[unsafe(no_mangle)]
pub static NSFontWeightLight: f64 = -0.4;
#[unsafe(no_mangle)]
pub static NSFontWeightRegular: f64 = 0.0;
#[unsafe(no_mangle)]
pub static NSFontWeightMedium: f64 = 0.23;
#[unsafe(no_mangle)]
pub static NSFontWeightSemibold: f64 = 0.3;
#[unsafe(no_mangle)]
pub static NSFontWeightBold: f64 = 0.4;
#[unsafe(no_mangle)]
pub static NSFontWeightHeavy: f64 = 0.56;
#[unsafe(no_mangle)]
pub static NSFontWeightBlack: f64 = 0.62;
