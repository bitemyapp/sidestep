//! objc2-app-kit's methods that take or return CoreGraphics, QuartzCore and
//! CoreText types, and those framework crates, build on Linux with objc2's
//! fork (tools/objc2-overlay, docs/abi.md). Sidestep implements
//! CoreGraphics (conformance/tests/coregraphics*.rs test it); QuartzCore
//! and CoreText aren't yet, so this only checks that everything
//! type-checks and links: nothing here sends a message or calls a
//! function. The crates'
//! Darwin-only `libc` items are compiled too, through the dev-dependencies'
//! `libc` features, so one that lost its Apple gate would fail the build.

#![cfg(not(target_vendor = "apple"))]

use std::marker::PhantomData;

use objc2_app_kit::{
    NSAnimationContext, NSColor, NSFont, NSFontDescriptor, NSGraphicsContext, NSImage, NSTextAlignment, NSView,
};
use objc2_core_text::{CTFont, CTFontDescriptor};

use sidestep_appkit as _;

#[test]
fn appkit_methods_with_framework_types() {
    // The methods apps call most, which objc2-app-kit 0.3.2 had only on
    // Apple platforms.
    let _ = NSColor::CGColor;
    let _ = NSColor::colorWithCGColor;
    let _ = NSView::layer;
    let _ = NSView::setLayer;
    let _ = NSView::makeBackingLayer;
    let _ = NSView::displayLinkWithTarget_selector;
    let _ = NSGraphicsContext::CGContext;
    let _ = NSGraphicsContext::graphicsContextWithCGContext_flipped;
    let _ = NSImage::initWithCGImage_size;
    let _ = NSImage::CGImageForProposedRect_context_hints;
    let _ = NSAnimationContext::setTimingFunction;
    let _ = NSFont::boundingRectForCGGlyph;
}

#[test]
fn framework_types() {
    let _: Option<&objc2_core_graphics::CGContext> = None;
    let _: Option<&objc2_core_graphics::CGPath> = None;
    let _: Option<&objc2_quartz_core::CALayer> = None;
    let _: Option<&objc2_quartz_core::CADisplayLink> = None;
    let _: Option<&objc2_core_text::CTLine> = None;
    let _: Option<&objc2_image_io::CGImageSource> = None;
}

/// Whether `T: AsRef<U>`, answered at compile time (an inherent constant
/// shadows the trait's where the bound holds).
struct Bridged<T: ?Sized, U: ?Sized>(PhantomData<T>, PhantomData<U>);

trait NotBridged {
    const HOLDS: bool = false;
}

impl<T: ?Sized, U: ?Sized> NotBridged for Bridged<T, U> {}

impl<T: ?Sized + AsRef<U>, U: ?Sized> Bridged<T, U> {
    const HOLDS: bool = true;
}

// GNUstep's (and Sidestep's) NSFont isn't a CTFont, so the toll-free
// bridging casts stay Apple-only. Checked while compiling.
const _: () = assert!(!<Bridged<NSFont, CTFont>>::HOLDS);
const _: () = assert!(!<Bridged<CTFont, NSFont>>::HOLDS);
const _: () = assert!(!<Bridged<NSFontDescriptor, CTFontDescriptor>>::HOLDS);
// The check itself works.
const _: () = assert!(<Bridged<NSFont, NSFont>>::HOLDS);

// GNUstep's NSTextAlignment values on every architecture (objc2-app-kit
// 0.3.2 gave x86_64 Linux macOS's, Center = 2 and Right = 1). Checked while
// compiling, so an x86_64 check or clippy run covers it.
const _: () = assert!(NSTextAlignment::Center.0 == 1 && NSTextAlignment::Right.0 == 2);
