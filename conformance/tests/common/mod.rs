//! Pixel helpers the drawing, color and image tests share.
//!
//! Tests draw into 8-bit RGBA bitmaps tagged sRGB, so that macOS doesn't
//! color-match sRGB colors on the way in, and read pixels back from
//! `bitmapData` (row 0 is the top of the image). Drawing goes through
//! `+[NSGraphicsContext graphicsContextWithBitmapImageRep:]` or a view's
//! `cacheDisplayInRect:toBitmapImageRep:`, so no window is needed.
#![allow(dead_code)]

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageRep, NSColorSpace, NSDeviceRGBColorSpace, NSGraphicsContext, NSResponder, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

pub fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

pub fn pt(x: f64, y: f64) -> NSPoint {
    NSPoint::new(x, y)
}

/// A `w` × `h` pixel bitmap: 8 bits per sample, RGBA, premultiplied,
/// tagged sRGB, every pixel transparent.
pub fn bitmap(w: isize, h: isize) -> Retained<NSBitmapImageRep> {
    // SAFETY: NULL planes make the rep allocate its own; the color space
    // name is AppKit's constant.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w,
            h,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::empty(),
            0,
            32,
        )
    }
    .expect("an RGBA bitmap");
    let rep = rep.bitmapImageRepByRetaggingWithColorSpace(&NSColorSpace::sRGBColorSpace()).expect("retagged");
    clear(&rep);
    rep
}

/// Zero every byte of `rep`'s pixels.
pub fn clear(rep: &NSBitmapImageRep) {
    let len = rep.bytesPerRow() as usize * rep.pixelsHigh() as usize;
    // SAFETY: bitmapData points at bytesPerRow × pixelsHigh bytes.
    unsafe { std::ptr::write_bytes(rep.bitmapData(), 0, len) };
}

/// The premultiplied RGBA bytes of the pixel `x` from the left and `y` from
/// the top.
pub fn pixel(rep: &NSBitmapImageRep, x: isize, y: isize) -> [u8; 4] {
    assert!(x >= 0 && y >= 0 && x < rep.pixelsWide() && y < rep.pixelsHigh(), "({x}, {y}) out of the bitmap");
    let at = y as usize * rep.bytesPerRow() as usize + x as usize * 4;
    // SAFETY: inside the bitmap, as checked.
    unsafe { std::ptr::read(rep.bitmapData().add(at).cast::<[u8; 4]>()) }
}

/// Fill every pixel of `rep` with premultiplied `rgba`.
pub fn fill_bitmap(rep: &NSBitmapImageRep, rgba: [u8; 4]) {
    let (w, h, row) = (rep.pixelsWide() as usize, rep.pixelsHigh() as usize, rep.bytesPerRow() as usize);
    for y in 0..h {
        for x in 0..w {
            // SAFETY: inside the bitmap.
            unsafe { std::ptr::write(rep.bitmapData().add(y * row + x * 4).cast::<[u8; 4]>(), rgba) };
        }
    }
}

/// Whether each channel of `a` is within 2 of `b`'s.
pub fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= 2)
}

#[track_caller]
pub fn assert_px(rep: &NSBitmapImageRep, x: isize, y: isize, want: [u8; 4]) {
    let got = pixel(rep, x, y);
    assert!(near(got, want), "pixel ({x}, {y}) is {got:?}, want {want:?}");
}

/// Run `f` with a bitmap context on `rep` current, restoring the context
/// that was current before.
pub fn draw_in<R>(rep: &NSBitmapImageRep, f: impl FnOnce(&NSGraphicsContext) -> R) -> R {
    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(rep).expect("a bitmap context");
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ctx));
    let r = f(&ctx);
    ctx.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    r
}

type Draw = Box<dyn Fn(&NSView, NSRect)>;

pub struct DrawIvars {
    flipped: bool,
    draw: RefCell<Option<Draw>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceDrawingView"]
    #[ivars = DrawIvars]
    pub struct DrawView;

    impl DrawView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            let view: &NSView = self;
            if let Some(f) = self.ivars().draw.borrow().as_ref() {
                f(view, dirty);
            }
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            self.ivars().flipped
        }
    }
);

/// A view that draws with `draw` (given itself and the dirty rectangle).
pub fn draw_view(
    mtm: MainThreadMarker,
    frame: NSRect,
    flipped: bool,
    draw: impl Fn(&NSView, NSRect) + 'static,
) -> Retained<NSView> {
    let this = DrawView::alloc(mtm).set_ivars(DrawIvars { flipped, draw: RefCell::new(Some(Box::new(draw))) });
    let view: Retained<DrawView> = unsafe { msg_send![super(this), initWithFrame: frame] };
    Retained::into_super(view)
}

/// `view`'s bounds drawn into a new bitmap at `scale` pixels per point.
pub fn snapshot(view: &NSView, scale: f64) -> Retained<NSBitmapImageRep> {
    let b = view.bounds();
    let rep = bitmap((b.size.width * scale) as isize, (b.size.height * scale) as isize);
    rep.setSize(b.size);
    view.cacheDisplayInRect_toBitmapImageRep(b, &rep);
    rep
}

pub const CLEAR: [u8; 4] = [0, 0, 0, 0];
pub const BLACK: [u8; 4] = [0, 0, 0, 255];
pub const WHITE: [u8; 4] = [255, 255, 255, 255];
pub const RED: [u8; 4] = [255, 0, 0, 255];
pub const GREEN: [u8; 4] = [0, 255, 0, 255];
pub const BLUE: [u8; 4] = [0, 0, 255, 255];
