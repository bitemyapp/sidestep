//! CoreGraphics, on Sidestep's drawing machinery: the C functions and types
//! objc2-core-graphics declares, and the AppKit methods that hand its
//! objects in and out (`-[NSColor CGColor]`, `-[NSGraphicsContext
//! CGContext]` and the rest, which live with their classes and call in
//! here).
//!
//! CoreFoundation objects are Objective-C objects in Sidestep, and so are
//! CoreGraphics': each type is a class of its own with a Sidestep-private
//! name (`_SidestepCGColor`, …), whose `…GetTypeID` is the ID
//! `CFGetTypeID` works out from that class (`sidestep-foundation`'s
//! `cf::types`). `CFRetain` and `CFRelease` are the runtime's retain and
//! release, `CFEqual`, `CFHash` and `CFCopyDescription` send `isEqual:`,
//! `hash` and `description`, which the classes answer as CoreGraphics
//! does, and objc2-core-foundation's `CFRetained` and downcasts work
//! through objc2-core-graphics' declarations unchanged. A Create or Copy
//! function returns a +1 reference; a Get function returns one its argument
//! keeps alive. The types, one module each:
//!
//! - [`geometry`]: `CGRect…`, `CGPoint…`, `CGSize…` and `CGAffineTransform…`
//!   functions and constants (plain values, no objects).
//! - [`color`]: `CGColorSpace` and `CGColor`. Colors are managed by
//!   formula, as `NSColor`'s are: spaces convert to sRGB by their model,
//!   transfer curve and primaries (Display P3's, Generic RGB's) when colors
//!   are drawn.
//! - [`path`]: `CGPath` and `CGMutablePath` (one class; an immutable path's
//!   shape is fixed and shared, a mutable one's is used by one thread at a
//!   time), with CoreGraphics' element structure: rectangles, ellipses,
//!   rounded rectangles and arcs are built as macOS builds them
//!   (`conformance/tests/coregraphics.rs` pins them).
//! - [`context`]: `CGContext`. A context holds a `context::ContextState`,
//!   the state `NSGraphicsContext` draws with too: an `NSGraphicsContext`
//!   wraps a CGContext, so drawing through either is the same drawing. The
//!   functions map onto the state one to one, with no message sends.
//! - [`bitmap`]: `CGBitmapContextCreate` and its getters: contexts drawing
//!   into memory in the layout asked for (`raster::pixels`).
//! - [`image`]: `CGImage`, its masks, and the pixels drawing takes of it.
//! - [`data`]: `CGDataProvider` and `CGDataConsumer`.
//! - [`gradient`]: `CGGradient`, `CGFunction` and `CGShading`.
//! - [`font`]: `CGFont`, over font files' tables (skrifa); text drawn
//!   through a CGContext (`CGContextShowGlyphs…`) goes through CoreText's
//!   glyph drawing (`coretext::draw`).
//!
//! CoreGraphics lives in `sidestep-appkit` because it shares AppKit's
//! graphics state, rasterizer, image cache and text: a CGContext *is*
//! AppKit's context state. Splitting it into a crate below AppKit would
//! take moving `context::ContextState`, `protocol`'s ops, `raster`, the
//! image pixels (`raster::images`) and `color`'s conversions with it, and
//! giving AppKit's classes (`NSColor`, `NSImage`, `NSBezierPath`) the
//! bridges as categories there; nothing here depends on AppKit's classes
//! but those bridges and the current context.
//!
//! Not here: PDF (`CGPDF…`); patterns (`CGPatternCreate` keeps its
//! callbacks, but a pattern color isn't made and setting a pattern leaves
//! the color as it was); conic gradients; the path set operations
//! (`CGPathCreateCopyByUnioningPath` and its kin, and
//! `CGPathCreateCopyByNormalizing`); CMYK and 5-bit bitmap contexts
//! (refused); and CGEvent, CGDisplay and CGWindow, which belong to the
//! window server.

pub(crate) mod bitmap;
pub(crate) mod color;
pub(crate) mod context;
pub(crate) mod data;
pub(crate) mod font;
pub(crate) mod geometry;
pub(crate) mod gradient;
pub(crate) mod image;
pub(crate) mod path;

use std::ptr::NonNull;

use objc2::Message;
use objc2::rc::Retained;

/// The parts of a bitmap info (`CGBitmapInfo`): its masks and the values
/// images and bitmap contexts look for.
pub(crate) mod info {
    use objc2_core_graphics::{CGBitmapInfo, CGImageByteOrderInfo, CGImageComponentInfo};

    pub const ALPHA_MASK: u32 = CGBitmapInfo::AlphaInfoMask.bits();
    pub const ORDER_MASK: u32 = CGBitmapInfo::ByteOrderInfoMask.bits();
    pub const FORMAT_MASK: u32 = CGBitmapInfo::PixelFormatInfoMask.bits();
    pub const FLOAT: u32 = CGImageComponentInfo::Float.0;
    pub const ORDER_16_LITTLE: u32 = CGImageByteOrderInfo::Order16Little.0;
    pub const ORDER_32_LITTLE: u32 = CGImageByteOrderInfo::Order32Little.0;
    pub const ORDER_16_BIG: u32 = CGImageByteOrderInfo::Order16Big.0;
    pub const ORDER_32_BIG: u32 = CGImageByteOrderInfo::Order32Big.0;
}

/// A +1 reference to `object`, as a Create or Copy function returns it, as
/// objc2-core-graphics' type `T`.
pub(crate) fn owned<I: Message, T>(object: Retained<I>) -> NonNull<T> {
    // SAFETY: into_raw never returns null.
    unsafe { NonNull::new_unchecked(Retained::into_raw(object).cast::<T>()) }
}

/// A reference to `object` its owner keeps alive, as a Get function returns
/// it.
pub(crate) fn borrowed<I, T>(object: &I) -> NonNull<T> {
    NonNull::from(object).cast::<T>()
}

/// Write `value` through `out` if it isn't null.
///
/// # Safety
///
/// `out` is null or valid to write a `T`.
pub(crate) unsafe fn store<T>(out: *mut T, value: T) {
    if !out.is_null() {
        // SAFETY: as the caller promises.
        unsafe { out.write(value) };
    }
}

/// `count` items at `items`, none for a null pointer.
///
/// # Safety
///
/// `items` is null or points at `count` readable items that outlive the
/// slice.
pub(crate) unsafe fn slice<'a, T>(items: *const T, count: usize) -> &'a [T] {
    if items.is_null() || count == 0 {
        &[]
    } else {
        // SAFETY: as the caller promises.
        unsafe { std::slice::from_raw_parts(items, count) }
    }
}

/// An object's description in CoreFoundation's style, `<Name 0x…>` and
/// what follows.
pub(crate) fn description<T>(name: &str, this: &T, rest: &str) -> Retained<objc2_foundation::NSString> {
    let at = this as *const T as usize;
    let text = if rest.is_empty() { format!("<{name} {at:#x}>") } else { format!("<{name} {at:#x}> {rest}") };
    objc2_foundation::NSString::from_str(&text)
}
