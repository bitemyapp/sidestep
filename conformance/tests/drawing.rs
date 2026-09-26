//! Drawing, checked by its pixels on macOS and on Linux alike: graphics
//! contexts and their state, the drawing functions, transforms, every
//! part of NSBezierPath, gradients and shadows. Pixels are read from 8-bit RGBA bitmaps tagged
//! sRGB (see `common`), drawn through bitmap contexts or a view's
//! `cacheDisplayInRect:toBitmapImageRep:`, so no window is needed.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

mod common;

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::Rc;

use common::*;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAffineTransformNSAppKitAdditions, NSBezierPath, NSBezierPathElement, NSBitmapFormat, NSBitmapImageRep, NSColor,
    NSColorRenderingIntent, NSColorSpace, NSColorSpaceModel, NSCompositingOperation, NSDeviceRGBColorSpace,
    NSDeviceWhiteColorSpace, NSDottedFrameRect, NSDrawWindowBackground, NSEraseRect, NSFrameRect, NSFrameRectWithWidth,
    NSFrameRectWithWidthUsingOperation, NSGradient, NSGradientDrawingOptions, NSGraphicsContext, NSImageInterpolation,
    NSLineCapStyle, NSLineJoinStyle, NSRectClip, NSRectClipList, NSRectFill, NSRectFillList,
    NSRectFillListUsingOperation, NSRectFillListWithColors, NSRectFillListWithColorsUsingOperation,
    NSRectFillListWithGrays, NSRectFillUsingOperation, NSShadow, NSWindingRule,
};
use objc2_app_kit::{NSFont, NSFontAttributeName, NSStringDrawing};
use objc2_foundation::{
    NSAffineTransform, NSAffineTransformStruct, NSArray, NSCopying, NSDictionary, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString,
};

use sidestep as _;

fn half_red() -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 0.5)
}

/// Whether a pixel is partly covered: neither untouched nor opaque.
fn partial(p: [u8; 4]) -> bool {
    p[3] > 20 && p[3] < 235
}

// Contexts.

fn context_lifetime(mtm: MainThreadMarker) {
    assert!(NSGraphicsContext::currentContext().is_none(), "no context outside drawing");
    let seen: Rc<RefCell<Vec<(bool, bool, bool)>>> = Rc::default();
    for flipped in [false, true] {
        let seen = seen.clone();
        let view = draw_view(mtm, rect(0.0, 0.0, 4.0, 4.0), flipped, move |_, _| {
            let a = NSGraphicsContext::currentContext().expect("a context in drawRect:");
            let b = NSGraphicsContext::currentContext().expect("a context in drawRect:");
            seen.borrow_mut().push((std::ptr::eq(&*a, &*b), a.isFlipped(), flipped));
        });
        snapshot(&view, 1.0);
    }
    assert_eq!(*seen.borrow(), [(true, false, false), (true, true, true)], "one context, flipped as the view");
    assert!(NSGraphicsContext::currentContext().is_none(), "restored after caching the display");
}

fn bitmap_contexts(mtm: MainThreadMarker) {
    let make = |bps: isize, spp: isize, alpha: bool, planar: bool, format: NSBitmapFormat, bpp: isize| {
        // SAFETY: NULL planes, and the color space name is a constant.
        unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                4,
                4,
                bps,
                spp,
                alpha,
                planar,
                if spp - isize::from(alpha) < 3 { NSDeviceWhiteColorSpace } else { NSDeviceRGBColorSpace },
                format,
                0,
                bpp,
            )
        }
        .expect("a rep")
    };
    let ctx = |r: &NSBitmapImageRep| NSGraphicsContext::graphicsContextWithBitmapImageRep(r).is_some();
    assert!(ctx(&make(8, 4, true, false, NSBitmapFormat::empty(), 32)), "premultiplied RGBA");
    assert!(!ctx(&make(8, 4, true, false, NSBitmapFormat::AlphaNonpremultiplied, 32)), "unpremultiplied");
    assert!(!ctx(&make(8, 3, false, false, NSBitmapFormat::empty(), 24)), "24-bit RGB");
    assert!(!ctx(&make(8, 4, true, true, NSBitmapFormat::empty(), 8)), "planar");
    // Contexts draw into other layouts too: gray, gray and alpha, RGB
    // padded to four bytes (a decoded opaque file's), alpha first, 16 bits
    // a sample.
    let gray = make(8, 1, false, false, NSBitmapFormat::empty(), 8);
    let gray_alpha = make(8, 2, true, false, NSBitmapFormat::empty(), 16);
    let rgbx = make(8, 3, false, false, NSBitmapFormat::empty(), 32);
    let argb = make(8, 4, true, false, NSBitmapFormat::AlphaFirst, 32);
    let wide = make(16, 4, true, false, NSBitmapFormat::empty(), 64);
    for (rep, what) in
        [(&gray, "gray"), (&gray_alpha, "gray and alpha"), (&rgbx, "RGBX"), (&argb, "ARGB"), (&wide, "16-bit")]
    {
        assert!(ctx(rep), "{what}");
    }
    let byte = |rep: &NSBitmapImageRep, x: usize, y: usize, i: usize| {
        // SAFETY: inside the bitmap.
        unsafe { *rep.bitmapData().add(y * rep.bytesPerRow() as usize + x * rep.bitsPerPixel() as usize / 8 + i) }
    };
    // White on the left half, black on the right: whole gray samples.
    for rep in [&gray, &gray_alpha] {
        let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(rep).expect("a context");
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&ctx));
        NSColor::whiteColor().setFill();
        NSRectFill(rect(0.0, 0.0, 2.0, 4.0));
        NSColor::blackColor().setFill();
        NSRectFill(rect(2.0, 0.0, 2.0, 4.0));
        NSGraphicsContext::restoreGraphicsState_class();
        assert_eq!([byte(rep, 0, 1, 0), byte(rep, 1, 1, 0), byte(rep, 2, 1, 0), byte(rep, 3, 1, 0)], [255, 255, 0, 0]);
    }
    assert_eq!((byte(&gray_alpha, 0, 1, 1), byte(&gray_alpha, 3, 1, 1)), (255, 255), "opaque");
    // Red, sRGB into sRGB: the samples in the rep's order.
    let srgb = NSColorSpace::sRGBColorSpace();
    let rgbx = rgbx.bitmapImageRepByRetaggingWithColorSpace(&srgb).expect("retagged");
    let argb = argb.bitmapImageRepByRetaggingWithColorSpace(&srgb).expect("retagged");
    for rep in [&rgbx, &argb] {
        draw_in(rep, |_| {
            NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0).setFill();
            NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
        });
    }
    assert_eq!([byte(&rgbx, 1, 1, 0), byte(&rgbx, 1, 1, 1), byte(&rgbx, 1, 1, 2)], [255, 0, 0]);
    assert_eq!([0, 1, 2, 3].map(|i| byte(&argb, 1, 1, i)), [255, 255, 0, 0]);
    let wide = wide.bitmapImageRepByRetaggingWithColorSpace(&srgb).expect("retagged");
    draw_in(&wide, |_| {
        NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0).setFill();
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    let sample = |i: usize| u16::from_ne_bytes([byte(&wide, 1, 1, 2 * i), byte(&wide, 1, 1, 2 * i + 1)]);
    assert_eq!([0, 1, 2, 3].map(sample), [65535, 0, 0, 65535]);
    let float = make(32, 4, true, false, NSBitmapFormat::FloatingPointSamples, 128);
    assert!(ctx(&float), "floating point");
    let float = float.bitmapImageRepByRetaggingWithColorSpace(&srgb).expect("retagged");
    draw_in(&float, |_| {
        NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.5, 0.0, 1.0).setFill();
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    let fsample = |i: usize| f32::from_ne_bytes([0, 1, 2, 3].map(|k| byte(&float, 1, 1, 4 * i + k)));
    let got = [0, 1, 2, 3].map(fsample);
    assert!(got.iter().zip([1.0, 0.5, 0.0, 1.0]).all(|(a, b)| (a - b).abs() < 0.004), "{got:?}");
    // A bitmap with no pixels yet has nothing to draw into.
    assert!(!ctx(&NSBitmapImageRep::initForIncrementalLoad(NSBitmapImageRep::alloc())));
    // A view cached into an RGBX bitmap.
    let view = draw_view(mtm, rect(0.0, 0.0, 4.0, 4.0), false, |_, _| {
        NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0).setFill();
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    view.cacheDisplayInRect_toBitmapImageRep(rect(0.0, 0.0, 4.0, 4.0), &rgbx);
    assert_eq!([byte(&rgbx, 2, 2, 0), byte(&rgbx, 2, 2, 1), byte(&rgbx, 2, 2, 2)], [0, 0, 255]);
    // Sizes whose bytes don't fit in memory make no bitmap.
    // SAFETY: NULL planes.
    let huge = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            1 << 31,
            1 << 31,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::empty(),
            0,
            32,
        )
    };
    assert!(huge.is_none(), "2³¹ × 2³¹ pixels");

    let rep = bitmap(4, 4);
    draw_in(&rep, |ctx| {
        assert!(!ctx.isFlipped(), "bitmap contexts are unflipped");
        assert!(ctx.shouldAntialias());
        assert_eq!(ctx.imageInterpolation(), NSImageInterpolation::Default);
        assert_eq!(ctx.compositingOperation(), NSCompositingOperation::SourceOver);
        assert_eq!(ctx.colorRenderingIntent(), NSColorRenderingIntent::RelativeColorimetric);
        assert_eq!(ctx.patternPhase(), NSPoint::ZERO);
        NSColor::redColor().setFill();
        NSRectFill(rect(0.0, 0.0, 1.0, 1.0));
        // In the pixels at once, before flushGraphics.
        assert_px(&rep, 0, 3, RED);
    });
    assert_px(&rep, 0, 0, CLEAR);
    // The origin is the bottom left: (0, 0) is in the last row.
    assert_px(&rep, 0, 3, RED);
}

fn fresh_state_per_view(mtm: MainThreadMarker) {
    let parent = draw_view(mtm, rect(0.0, 0.0, 8.0, 8.0), false, |_, _| {});
    // The first view leaves a color, a transform and a clip behind...
    let first = draw_view(mtm, rect(0.0, 0.0, 4.0, 8.0), false, |_, _| {
        NSColor::redColor().setFill();
        let t = NSAffineTransform::transform();
        t.translateXBy_yBy(1.0, 0.0);
        t.concat();
        NSRectClip(rect(0.0, 0.0, 1.0, 1.0));
        NSGraphicsContext::currentContext().unwrap().saveGraphicsState();
    });
    // ...which its sibling doesn't see: it fills in the default color, black.
    let second = draw_view(mtm, rect(4.0, 0.0, 4.0, 8.0), false, |_, _| NSRectFill(rect(0.0, 0.0, 4.0, 8.0)));
    parent.addSubview(&first);
    parent.addSubview(&second);
    let rep = snapshot(&parent, 1.0);
    for (x, y) in [(4, 0), (7, 7), (5, 4)] {
        assert_px(&rep, x, y, BLACK);
    }
}

fn save_and_restore(_: MainThreadMarker) {
    let rep = bitmap(4, 4);
    draw_in(&rep, |ctx| {
        NSColor::redColor().setFill();
        ctx.saveGraphicsState();
        NSColor::blueColor().setFill();
        let t = NSAffineTransform::transform();
        t.translateXBy_yBy(2.0, 0.0);
        t.concat();
        NSRectClip(rect(0.0, 0.0, 1.0, 1.0));
        ctx.restoreGraphicsState();
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    // Color, transform and clip all came back.
    assert_px(&rep, 0, 0, RED);
    assert_px(&rep, 3, 3, RED);
    // The class methods also restore which context is current.
    let rep = bitmap(1, 1);
    draw_in(&rep, |_| {
        let before = NSGraphicsContext::currentContext().unwrap();
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(None);
        assert!(NSGraphicsContext::currentContext().is_none());
        NSGraphicsContext::restoreGraphicsState_class();
        assert!(std::ptr::eq(&*NSGraphicsContext::currentContext().unwrap(), &*before));
    });
}

// Compositing.

fn rect_fill_copies(_: MainThreadMarker) {
    let rep = bitmap(4, 4);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| {
        half_red().setFill();
        NSRectFill(rect(0.0, 0.0, 1.0, 4.0));
        NSRectFillUsingOperation(rect(1.0, 0.0, 1.0, 4.0), NSCompositingOperation::Copy);
        NSRectFillUsingOperation(rect(2.0, 0.0, 1.0, 4.0), NSCompositingOperation::SourceOver);
        NSBezierPath::fillRect(rect(3.0, 0.0, 1.0, 4.0));
    });
    // NSRectFill replaces what's there, alpha included.
    assert_px(&rep, 0, 0, [128, 0, 0, 128]);
    assert_px(&rep, 1, 0, [128, 0, 0, 128]);
    // Source over blends; so does +fillRect:, in the context's operation.
    assert_px(&rep, 2, 0, [128, 0, 127, 255]);
    assert_px(&rep, 3, 0, [128, 0, 127, 255]);

    let rep = bitmap(4, 4);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| {
        NSColor::redColor().setFill();
        NSRectFillUsingOperation(rect(0.0, 0.0, 1.0, 4.0), NSCompositingOperation::Clear);
        NSRectFillUsingOperation(rect(1.0, 0.0, 1.0, 4.0), NSCompositingOperation::DestinationOut);
    });
    assert_px(&rep, 0, 0, CLEAR);
    assert_px(&rep, 1, 0, CLEAR);
    assert_px(&rep, 2, 0, BLUE);

    // The context's operation applies to paths and +fillRect:.
    let rep = bitmap(4, 4);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |ctx| {
        ctx.setCompositingOperation(NSCompositingOperation::Copy);
        assert_eq!(ctx.compositingOperation(), NSCompositingOperation::Copy);
        half_red().setFill();
        NSBezierPath::fillRect(rect(0.0, 0.0, 1.0, 4.0));
        NSBezierPath::bezierPathWithRect(rect(1.0, 0.0, 1.0, 4.0)).fill();
    });
    assert_px(&rep, 0, 0, [128, 0, 0, 128]);
    assert_px(&rep, 1, 0, [128, 0, 0, 128]);
    assert_px(&rep, 2, 0, BLUE);
}

fn antialiasing(_: MainThreadMarker) {
    let rep = bitmap(4, 4);
    draw_in(&rep, |_| {
        NSColor::blackColor().setFill();
        NSRectFill(rect(0.5, 0.0, 1.0, 1.0));
        NSBezierPath::bezierPathWithRect(rect(0.5, 1.0, 1.0, 1.0)).fill();
    });
    for y in [3, 2] {
        assert!(partial(pixel(&rep, 0, y)) && partial(pixel(&rep, 1, y)), "row {y}: edges partly covered");
    }
    let rep = bitmap(4, 4);
    draw_in(&rep, |ctx| {
        ctx.setShouldAntialias(false);
        assert!(!ctx.shouldAntialias());
        NSColor::blackColor().setFill();
        NSRectFill(rect(0.5, 0.0, 1.0, 1.0));
        NSBezierPath::bezierPathWithRect(rect(0.4, 1.0, 1.0, 1.0)).fill();
        NSBezierPath::bezierPathWithRect(rect(0.6, 2.0, 1.0, 1.0)).fill();
    });
    for y in 1..4 {
        for x in 0..4 {
            let a = pixel(&rep, x, y)[3];
            assert!(a == 0 || a == 255, "({x}, {y}) is all or nothing, not {a}");
        }
    }
}

fn frames_erasing_and_clips(_: MainThreadMarker) {
    let rep = bitmap(8, 8);
    draw_in(&rep, |_| {
        NSColor::blackColor().setFill();
        NSFrameRect(rect(1.0, 1.0, 6.0, 6.0));
    });
    // Inside the rectangle: its outermost pixels, not the ones beyond.
    assert_px(&rep, 1, 1, BLACK);
    assert_px(&rep, 6, 6, BLACK);
    assert_px(&rep, 1, 4, BLACK);
    assert_px(&rep, 0, 4, CLEAR);
    assert_px(&rep, 2, 4, CLEAR);
    assert_px(&rep, 7, 4, CLEAR);
    let rep = bitmap(8, 8);
    draw_in(&rep, |_| {
        NSColor::blackColor().setFill();
        NSFrameRectWithWidth(rect(1.0, 1.0, 6.0, 6.0), 2.0);
    });
    assert_px(&rep, 2, 4, BLACK);
    assert_px(&rep, 3, 4, CLEAR);
    assert_px(&rep, 0, 4, CLEAR);

    let rep = bitmap(4, 4);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| NSEraseRect(rect(0.0, 0.0, 2.0, 4.0)));
    assert_px(&rep, 0, 0, WHITE);
    assert_px(&rep, 2, 0, BLUE);

    let rep = bitmap(4, 4);
    draw_in(&rep, |_| {
        NSColor::redColor().setFill();
        NSRectClip(rect(0.0, 0.0, 2.0, 4.0));
        NSRectClip(rect(1.0, 0.0, 3.0, 4.0));
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    // Clips only narrow.
    assert_eq!((0..4).map(|x| pixel(&rep, x, 0)).collect::<Vec<_>>(), [CLEAR, RED, CLEAR, CLEAR]);
}

// NSHighlightRect is deprecated, and pinned all the same.
#[allow(deprecated)]
fn fill_lists(_: MainThreadMarker) {
    let rects = [rect(0.0, 0.0, 1.0, 4.0), rect(2.0, 0.0, 1.0, 4.0)];
    let list = NonNull::from(&rects).cast::<NSRect>();
    let row = |rep: &NSBitmapImageRep| (0..4).map(|x| pixel(rep, x, 0)).collect::<Vec<_>>();
    // The list forms copy, as NSRectFill does, in the current color.
    let rep = bitmap(4, 4);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| {
        half_red().setFill();
        // SAFETY: two rectangles.
        unsafe { NSRectFillList(list, 2) };
    });
    let half = [128, 0, 0, 128];
    assert!(row(&rep).iter().zip([half, BLUE, half, BLUE]).all(|(a, b)| near(*a, b)), "{:?}", row(&rep));
    let rep = bitmap(4, 4);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| {
        half_red().setFill();
        // SAFETY: two rectangles.
        unsafe { NSRectFillListUsingOperation(list, 2, NSCompositingOperation::SourceOver) };
    });
    assert_px(&rep, 0, 0, [128, 0, 127, 255]);
    assert_px(&rep, 1, 0, BLUE);
    // Grays and colors, one a rectangle; the current color stays.
    let rep = bitmap(4, 4);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| {
        NSColor::redColor().setFill();
        let grays = [0.0, 1.0];
        // SAFETY: two rectangles and two grays.
        unsafe { NSRectFillListWithGrays(list, NonNull::from(&grays).cast(), 2) };
        NSRectFill(rect(3.0, 0.0, 1.0, 4.0));
    });
    assert_eq!(row(&rep), [BLACK, BLUE, WHITE, RED]);
    let rep = bitmap(4, 4);
    fill_bitmap(&rep, BLUE);
    let (green, half) = (NSColor::greenColor(), half_red());
    let colors = [NonNull::from(&*green), NonNull::from(&*half)];
    draw_in(&rep, |_| {
        NSColor::whiteColor().setFill();
        // SAFETY: two rectangles and two colors.
        unsafe { NSRectFillListWithColors(list, NonNull::from(&colors).cast(), 2) };
        NSRectFill(rect(3.0, 0.0, 1.0, 4.0));
    });
    assert_px(&rep, 0, 0, GREEN);
    assert_px(&rep, 2, 0, [128, 0, 0, 128]);
    assert_px(&rep, 3, 0, WHITE);
    let rep = bitmap(4, 4);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| {
        // SAFETY: two rectangles and two colors.
        unsafe {
            NSRectFillListWithColorsUsingOperation(
                list,
                NonNull::from(&colors).cast(),
                2,
                NSCompositingOperation::SourceOver,
            )
        };
    });
    assert_px(&rep, 2, 0, [128, 0, 127, 255]);
    // A frame by an operation.
    let rep = bitmap(8, 8);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| NSFrameRectWithWidthUsingOperation(rect(1.0, 1.0, 6.0, 6.0), 1.0, NSCompositingOperation::Clear));
    assert_px(&rep, 1, 4, CLEAR);
    assert_px(&rep, 0, 4, BLUE);
    assert_px(&rep, 2, 4, BLUE);
    // Clip lists: none leaves the clip as it was; several clip to their
    // union.
    let rep = bitmap(4, 4);
    draw_in(&rep, |_| {
        NSColor::redColor().setFill();
        // SAFETY: no rectangles.
        unsafe { NSRectClipList(NonNull::dangling(), 0) };
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    assert_eq!(row(&rep), [RED; 4]);
    let rep = bitmap(4, 4);
    draw_in(&rep, |_| {
        NSColor::redColor().setFill();
        // SAFETY: two rectangles.
        unsafe { NSRectClipList(list, 2) };
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    assert_eq!(row(&rep), [RED, CLEAR, RED, CLEAR]);
    // A dotted frame is a solid one pixel wide, these days.
    let rep = bitmap(10, 10);
    draw_in(&rep, |_| {
        NSColor::blackColor().setFill();
        NSDottedFrameRect(rect(1.0, 1.0, 8.0, 8.0));
    });
    for i in 1..9 {
        for (x, y) in [(i, 1), (i, 8), (1, i), (8, i)] {
            assert!(pixel(&rep, x, y)[3] > 0, "({x}, {y}) on the frame");
        }
        assert_px(&rep, i, 0, CLEAR);
        assert_px(&rep, 0, i, CLEAR);
    }
    assert_px(&rep, 4, 4, CLEAR);
    // The window background is opaque; a highlight leaves the pixels be.
    let rep = bitmap(4, 4);
    draw_in(&rep, |_| NSDrawWindowBackground(rect(0.0, 0.0, 2.0, 4.0)));
    assert_eq!(pixel(&rep, 0, 0)[3], 255);
    assert_px(&rep, 3, 0, CLEAR);
    let rep = bitmap(4, 4);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| objc2_app_kit::NSHighlightRect(rect(0.0, 0.0, 4.0, 4.0)));
    assert_px(&rep, 1, 1, BLUE);
}

fn copies(_: MainThreadMarker) {
    // Colors and gradients don't change: a copy is equal (and may be the
    // color itself).
    let c = NSColor::colorWithSRGBRed_green_blue_alpha(0.1, 0.2, 0.3, 0.4);
    assert!(c.copy().isEqual(Some(&c)));
    let label = NSColor::labelColor();
    assert!(label.copy().isEqual(Some(&label)));
    let g =
        NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &NSColor::redColor(), &NSColor::blueColor())
            .expect("a gradient");
    assert_eq!(g.copy().numberOfColorStops(), 2);
    // A path's copy has its elements and settings, and changes alone.
    let p = NSBezierPath::bezierPathWithRect(rect(0.0, 0.0, 4.0, 4.0));
    p.setLineWidth(3.0);
    p.setWindingRule(NSWindingRule::EvenOdd);
    let dash = [2.0, 1.0];
    // SAFETY: two lengths.
    unsafe { p.setLineDash_count_phase(dash.as_ptr(), 2, 0.5) };
    let q = p.copy();
    assert!(!std::ptr::eq(&*p, &*q));
    // (AppKit's copy may add a move after the close.)
    assert_eq!(elements(&q)[..5], elements(&p)[..]);
    assert_eq!(q.bounds(), p.bounds());
    assert_eq!((q.lineWidth(), q.windingRule()), (3.0, NSWindingRule::EvenOdd));
    let mut count = 0;
    // SAFETY: a NULL pattern asks only for the count.
    unsafe { q.getLineDash_count_phase(std::ptr::null_mut(), &mut count, std::ptr::null_mut()) };
    assert_eq!(count, 2);
    q.lineToPoint(pt(9.0, 9.0));
    q.setLineWidth(1.0);
    assert_eq!((p.elementCount(), p.lineWidth()), (5, 3.0));
    // A shadow's copy has its settings, and changes alone.
    let s = NSShadow::new();
    s.setShadowOffset(NSSize::new(2.0, -3.0));
    s.setShadowBlurRadius(4.0);
    s.setShadowColor(Some(&NSColor::redColor()));
    let t = s.copy();
    assert!(!std::ptr::eq(&*s, &*t));
    assert_eq!((t.shadowOffset(), t.shadowBlurRadius()), (NSSize::new(2.0, -3.0), 4.0));
    assert!(t.shadowColor().expect("a color").isEqual(Some(&NSColor::redColor())));
    t.setShadowBlurRadius(1.0);
    assert_eq!(s.shadowBlurRadius(), 4.0);
}

// Transforms.

fn transform_math(_: MainThreadMarker) {
    let close = |a: NSPoint, b: NSPoint| (a.x - b.x).abs() < 1e-12 && (a.y - b.y).abs() < 1e-12;
    let t = NSAffineTransform::transform();
    t.translateXBy_yBy(10.0, 0.0);
    t.scaleBy(2.0);
    // The step made last applies to points first.
    assert_eq!(t.transformPoint(pt(1.0, 1.0)), pt(12.0, 2.0));
    let r = NSAffineTransform::transform();
    r.rotateByDegrees(90.0);
    assert!(close(r.transformPoint(pt(1.0, 0.0)), pt(0.0, 1.0)));
    let s = r.transformStruct();
    assert!((s.m12 - 1.0).abs() < 1e-12 && (s.m21 + 1.0).abs() < 1e-12, "{s:?}");
    let radians = NSAffineTransform::transform();
    radians.rotateByRadians(std::f64::consts::FRAC_PI_2);
    assert!(close(radians.transformPoint(pt(1.0, 0.0)), pt(0.0, 1.0)));

    let a = NSAffineTransform::transform();
    a.translateXBy_yBy(10.0, 0.0);
    let b = NSAffineTransform::transform();
    b.scaleBy(2.0);
    let appended = NSAffineTransform::initWithTransform(NSAffineTransform::alloc(), &a);
    appended.appendTransform(&b);
    let prepended = NSAffineTransform::initWithTransform(NSAffineTransform::alloc(), &a);
    prepended.prependTransform(&b);
    // Appended: after this one; prepended: before it.
    assert_eq!(appended.transformPoint(pt(1.0, 1.0)), pt(22.0, 2.0));
    assert_eq!(prepended.transformPoint(pt(1.0, 1.0)), pt(12.0, 2.0));
    assert_eq!(a.transformPoint(pt(1.0, 1.0)), pt(11.0, 1.0), "initWithTransform: copied");
    assert_eq!(appended.transformSize(NSSize::new(1.0, 1.0)), NSSize::new(2.0, 2.0), "sizes ignore translation");
    let st = appended.transformStruct();
    assert_eq!((st.m11, st.m12, st.m21, st.m22, st.tX, st.tY), (2.0, 0.0, 0.0, 2.0, 20.0, 0.0));

    let t = NSAffineTransform::transform();
    t.scaleXBy_yBy(2.0, 4.0);
    t.translateXBy_yBy(1.0, 1.0);
    t.invert();
    let st = t.transformStruct();
    assert_eq!((st.m11, st.m22, st.tX, st.tY), (0.5, 0.25, -1.0, -1.0));
    let set = NSAffineTransform::transform();
    set.setTransformStruct(NSAffineTransformStruct { m11: 1.0, m12: 0.0, m21: 0.0, m22: 1.0, tX: 3.0, tY: 4.0 });
    assert_eq!(set.transformPoint(NSPoint::ZERO), pt(3.0, 4.0));
}

fn transforms_move_drawing(_: MainThreadMarker) {
    let rep = bitmap(4, 4);
    draw_in(&rep, |_| {
        let t = NSAffineTransform::transform();
        t.translateXBy_yBy(2.0, 1.0);
        t.concat();
        NSColor::redColor().setFill();
        NSRectFill(rect(0.0, 0.0, 1.0, 1.0));
    });
    assert_px(&rep, 2, 2, RED);
    assert_px(&rep, 0, 3, CLEAR);
    // -set replaces the transform: identity puts (0, 0) at the context's
    // own origin, the bitmap's bottom left.
    let rep = bitmap(4, 4);
    draw_in(&rep, |_| {
        let t = NSAffineTransform::transform();
        t.translateXBy_yBy(2.0, 2.0);
        t.concat();
        NSAffineTransform::transform().set();
        NSColor::redColor().setFill();
        NSRectFill(rect(0.0, 0.0, 1.0, 1.0));
    });
    assert_px(&rep, 0, 3, RED);
    // transformBezierPath: makes a new path and leaves this one.
    let p = NSBezierPath::bezierPathWithRect(rect(0.0, 0.0, 1.0, 1.0));
    let t = NSAffineTransform::transform();
    t.translateXBy_yBy(5.0, 0.0);
    let q = t.transformBezierPath(&p);
    assert_eq!((p.bounds(), q.bounds()), (rect(0.0, 0.0, 1.0, 1.0), rect(5.0, 0.0, 1.0, 1.0)));
    p.transformUsingAffineTransform(&t);
    assert_eq!(p.bounds(), rect(5.0, 0.0, 1.0, 1.0), "transformUsingAffineTransform: changes the path");
}

// Views.

fn rects_being_drawn(mtm: MainThreadMarker) {
    // The dirty rectangle, the rectangles being drawn, and needsToDrawRect:
    // inside and outside them.
    type Seen = Option<(NSRect, Vec<NSRect>, bool, bool)>;
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let s = seen.clone();
    let view = draw_view(mtm, rect(0.0, 0.0, 10.0, 10.0), false, move |v, dirty| {
        let mut rects: *const NSRect = std::ptr::null();
        let mut count: isize = 0;
        // SAFETY: both pointers are writable.
        unsafe { v.getRectsBeingDrawn_count(&mut rects, &mut count) };
        // SAFETY: AppKit hands back `count` rectangles.
        let list = (0..count).map(|i| unsafe { *rects.offset(i) }).collect();
        let inside = v.needsToDrawRect(rect(3.0, 3.0, 1.0, 1.0));
        let outside = v.needsToDrawRect(rect(8.0, 8.0, 1.0, 1.0));
        *s.borrow_mut() = Some((dirty, list, inside, outside));
    });
    let rep = bitmap(10, 10);
    view.cacheDisplayInRect_toBitmapImageRep(rect(2.0, 2.0, 4.0, 4.0), &rep);
    let (dirty, list, inside, outside) = seen.borrow().clone().expect("drawn");
    assert_eq!(dirty, rect(2.0, 2.0, 4.0, 4.0));
    assert_eq!(list, [rect(2.0, 2.0, 4.0, 4.0)]);
    assert!(inside && !outside);
}

fn view_snapshots(mtm: MainThreadMarker) {
    let view = draw_view(mtm, rect(0.0, 0.0, 10.0, 7.0), false, |_, _| {});
    let rep = view.bitmapImageRepForCachingDisplayInRect(rect(0.0, 0.0, 10.0, 7.0)).expect("a rep");
    assert_eq!(rep.size(), NSSize::new(10.0, 7.0));
    assert!(rep.pixelsWide() >= 10 && rep.pixelsWide() % 10 == 0 && rep.pixelsHigh() * 10 == rep.pixelsWide() * 7);
    assert!(NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).is_some(), "one a context draws into");

    // Subviews are drawn, hidden ones skipped, all within the rectangle.
    let parent = draw_view(mtm, rect(0.0, 0.0, 8.0, 8.0), false, |_, _| {
        NSColor::blueColor().setFill();
        NSRectFill(rect(0.0, 0.0, 8.0, 8.0));
    });
    let shown = draw_view(mtm, rect(0.0, 0.0, 4.0, 4.0), false, |_, _| {
        NSColor::redColor().setFill();
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    let hidden = draw_view(mtm, rect(4.0, 4.0, 4.0, 4.0), false, |_, _| {
        NSColor::greenColor().setFill();
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    hidden.setHidden(true);
    parent.addSubview(&shown);
    parent.addSubview(&hidden);
    let rep = snapshot(&parent, 1.0);
    assert_px(&rep, 1, 6, RED);
    assert_px(&rep, 5, 1, BLUE);
    // Part of the view: the rectangle fills the bitmap.
    let rep = bitmap(4, 4);
    parent.cacheDisplayInRect_toBitmapImageRep(rect(2.0, 2.0, 4.0, 4.0), &rep);
    assert_px(&rep, 0, 3, RED);
    assert_px(&rep, 3, 0, BLUE);
    // Twice the pixels: the same picture, finer.
    let rep = snapshot(&parent, 2.0);
    assert_px(&rep, 2, 12, RED);
    assert_px(&rep, 10, 2, BLUE);

    // Opacity: a snapshot leaves partial alpha out and subviews at 0 out,
    // and draws its receiver even at 0.
    hidden.setHidden(false);
    shown.setAlphaValue(0.5);
    hidden.setAlphaValue(0.0);
    let rep = snapshot(&parent, 1.0);
    assert_px(&rep, 1, 6, RED);
    assert_px(&rep, 5, 1, BLUE);
    parent.setAlphaValue(0.0);
    let rep = snapshot(&parent, 1.0);
    assert_px(&rep, 5, 5, BLUE);
    assert_px(&rep, 1, 6, RED);
    // Values outside 0 to 1 are kept as given.
    parent.setAlphaValue(2.0);
    assert_eq!(parent.alphaValue(), 2.0);
    parent.setAlphaValue(-1.0);
    assert_eq!(parent.alphaValue(), -1.0);
    parent.setAlphaValue(1.0);
}

// Paths.

type Element = (usize, Vec<(f64, f64)>);

fn elements(p: &NSBezierPath) -> Vec<Element> {
    (0..p.elementCount())
        .map(|i| {
            let mut pts = [NSPoint::ZERO; 3];
            // SAFETY: room for the three points an element has at most.
            let kind = unsafe { p.elementAtIndex_associatedPoints(i, pts.as_mut_ptr()) };
            let n = match kind {
                NSBezierPathElement::MoveTo | NSBezierPathElement::LineTo => 1,
                NSBezierPathElement::CubicCurveTo => 3,
                NSBezierPathElement::QuadraticCurveTo => 2,
                _ => 0,
            };
            let round = |v: f64| (v * 1000.0).round() / 1000.0;
            (kind.0, pts[..n].iter().map(|p| (round(p.x), round(p.y))).collect())
        })
        .collect()
}

fn kinds(p: &NSBezierPath) -> Vec<usize> {
    elements(p).into_iter().map(|(k, _)| k).collect()
}

const MOVE: usize = 0;
const LINE: usize = 1;
const CURVE: usize = 2;
const CLOSE: usize = 3;
const QUAD: usize = 4;

fn path_structure(_: MainThreadMarker) {
    // A rectangle: from its origin, counterclockwise, closed.
    let r = NSBezierPath::bezierPathWithRect(rect(1.0, 2.0, 10.0, 20.0));
    assert_eq!(
        elements(&r),
        [
            (MOVE, vec![(1.0, 2.0)]),
            (LINE, vec![(11.0, 2.0)]),
            (LINE, vec![(11.0, 22.0)]),
            (LINE, vec![(1.0, 22.0)]),
            (CLOSE, vec![]),
        ]
    );
    // An oval: four curves counterclockwise from the bottom right, not
    // closed.
    let o = NSBezierPath::bezierPathWithOvalInRect(rect(0.0, 0.0, 10.0, 20.0));
    let els = elements(&o);
    assert_eq!(kinds(&o), [MOVE, CURVE, CURVE, CURVE, CURVE]);
    assert_eq!(els[0].1, [(8.536, 2.929)]);
    assert_eq!(els[1].1[2], (8.536, 17.071));
    assert_eq!(els[4].1[2], (8.536, 2.929));
    // A rounded rectangle: from the top edge's left end, closed, then back.
    let rr = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect(0.0, 0.0, 10.0, 20.0), 2.0, 3.0);
    let els = elements(&rr);
    assert_eq!(kinds(&rr), [MOVE, CURVE, LINE, CURVE, LINE, CURVE, LINE, CURVE, CLOSE, MOVE]);
    assert_eq!(els[0].1, [(2.0, 20.0)]);
    assert_eq!(els[1].1, [(0.895, 20.0), (0.0, 18.657), (0.0, 17.0)]);
    assert_eq!(els[2].1, [(0.0, 3.0)]);
    assert_eq!(els[9].1, [(2.0, 20.0)]);
    // Radii beyond half a side are clamped to it.
    let big = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect(0.0, 0.0, 10.0, 20.0), 8.0, 30.0);
    let els = elements(&big);
    assert_eq!((els[0].1[0], els[1].1[2], els[4].1[0]), ((5.0, 20.0), (0.0, 10.0), (5.0, 0.0)));
    // No radii: a closed rectangle from the origin.
    let flat = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect(0.0, 0.0, 10.0, 20.0), 0.0, 0.0);
    assert_eq!(kinds(&flat), [MOVE, LINE, LINE, LINE, CLOSE, MOVE]);
    assert_eq!(elements(&flat)[0].1, [(0.0, 0.0)]);
    // curveToPoint:controlPoint: is quadratic: the control, then the end.
    let q = NSBezierPath::bezierPath();
    q.moveToPoint(pt(0.0, 0.0));
    q.curveToPoint_controlPoint(pt(10.0, 0.0), pt(5.0, 5.0));
    assert_eq!(elements(&q)[1], (QUAD, vec![(5.0, 5.0), (10.0, 0.0)]));
    assert_eq!(q.elementAtIndex(1), NSBezierPathElement::QuadraticCurveTo);
    // Points in, as moves and lines.
    let pts = [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0)];
    let p = NSBezierPath::bezierPath();
    // SAFETY: three points.
    unsafe { p.appendBezierPathWithPoints_count(pts.as_ptr().cast_mut(), 3) };
    assert_eq!(kinds(&p), [MOVE, LINE, LINE]);
    // setAssociatedPoints:atIndex: moves an element's points.
    let mut moved = [pt(7.0, 8.0)];
    // SAFETY: one point for a line.
    unsafe { p.setAssociatedPoints_atIndex(moved.as_mut_ptr(), 1) };
    assert_eq!(elements(&p)[1].1, [(7.0, 8.0)]);
    // After a point, points continue the subpath with lines.
    let more = [pt(10.0, 0.0), pt(10.0, 10.0)];
    let p = NSBezierPath::bezierPath();
    p.moveToPoint(pt(0.0, 0.0));
    p.lineToPoint(pt(5.0, 0.0));
    // SAFETY: two points.
    unsafe { p.appendBezierPathWithPoints_count(more.as_ptr().cast_mut(), 2) };
    assert_eq!(
        elements(&p),
        [(MOVE, vec![(0.0, 0.0)]), (LINE, vec![(5.0, 0.0)]), (LINE, vec![(10.0, 0.0)]), (LINE, vec![(10.0, 10.0)])]
    );
    let p = NSBezierPath::bezierPath();
    p.moveToPoint(pt(0.0, 0.0));
    p.lineToPoint(pt(5.0, 0.0));
    p.closePath();
    // SAFETY: two points.
    unsafe { p.appendBezierPathWithPoints_count(more.as_ptr().cast_mut(), 2) };
    assert_eq!(kinds(&p), [MOVE, LINE, CLOSE, MOVE, LINE, LINE]);
    assert_eq!(elements(&p)[3].1, [(0.0, 0.0)]);
    // So a move and points make a polygon, which fills.
    let tri = NSBezierPath::bezierPath();
    tri.moveToPoint(pt(0.0, 0.0));
    let rest = [pt(10.0, 0.0), pt(0.0, 10.0)];
    // SAFETY: two points.
    unsafe { tri.appendBezierPathWithPoints_count(rest.as_ptr().cast_mut(), 2) };
    assert_eq!(kinds(&tri), [MOVE, LINE, LINE]);
    assert!(tri.containsPoint(pt(2.0, 2.0)));
    // A rounded rectangle of negative width or height is no path at all.
    for r in [rect(0.0, 0.0, -10.0, 20.0), rect(0.0, 0.0, 10.0, -20.0)] {
        assert_eq!(NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 2.0, 2.0).elementCount(), 0, "{r:?}");
        let p = NSBezierPath::bezierPath();
        p.appendBezierPathWithRoundedRect_xRadius_yRadius(r, 2.0, 2.0);
        assert!(p.isEmpty());
    }
}

fn close_path(_: MainThreadMarker) {
    let p = NSBezierPath::bezierPath();
    p.moveToPoint(pt(1.0, 1.0));
    p.lineToPoint(pt(5.0, 1.0));
    p.closePath();
    // A close, then a move back to the start.
    assert_eq!(kinds(&p), [MOVE, LINE, CLOSE, MOVE]);
    assert_eq!(p.currentPoint(), pt(1.0, 1.0));
    p.lineToPoint(pt(5.0, 5.0));
    assert_eq!(p.elementCount(), 5, "no further move before the line");
    p.relativeLineToPoint(pt(1.0, 1.0));
    assert_eq!(p.currentPoint(), pt(6.0, 6.0));
    p.relativeMoveToPoint(pt(1.0, 0.0));
    p.relativeCurveToPoint_controlPoint1_controlPoint2(pt(2.0, 0.0), pt(0.0, 1.0), pt(2.0, 1.0));
    assert_eq!(elements(&p).last().unwrap(), &(CURVE, vec![(7.0, 7.0), (9.0, 7.0), (9.0, 6.0)]));
}

fn arcs(_: MainThreadMarker) {
    let arc = |start: f64, end: f64, clockwise: bool| {
        let p = NSBezierPath::bezierPath();
        p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
            pt(0.0, 0.0),
            10.0,
            start,
            end,
            clockwise,
        );
        p
    };
    // Degrees; a move to the start on an empty path; up to 90° a curve.
    for (start, end, clockwise, curves) in [
        (0.0, 90.0, false, 1),
        (0.0, 180.0, false, 2),
        (0.0, 360.0, false, 4),
        (0.0, 45.0, false, 1),
        (90.0, 0.0, false, 3),
        (0.0, 90.0, true, 3),
        (0.0, 45.0, true, 4),
        (90.0, 0.0, true, 1),
        (0.0, 360.0, true, 0),
        // More than a turn is more than a circle.
        (0.0, 450.0, false, 5),
        (0.0, 720.0, false, 8),
        (0.0, -450.0, true, 5),
        (0.0, -720.0, true, 8),
        (0.0, 540.0, true, 2),
        // A turn that rounds past 360 is a circle and a speck.
        (0.0, 36.0 * 10.000000000000002, false, 5),
    ] {
        let p = arc(start, end, clockwise);
        let k = kinds(&p);
        assert_eq!(k[0], MOVE);
        assert_eq!(k.iter().filter(|&&k| k == CURVE).count(), curves, "{start}→{end} clockwise {clockwise}");
        let e = end.to_radians();
        let cur = p.currentPoint();
        assert!((cur.x - 10.0 * e.cos()).abs() < 1e-9 && (cur.y - 10.0 * e.sin()).abs() < 1e-9, "ends at {end}°");
    }
    // Counterclockwise from 0 to 90 passes through positive y; clockwise
    // through negative.
    assert_eq!(elements(&arc(0.0, 90.0, false))[1].1[0], (10.0, 5.523));
    assert_eq!(elements(&arc(0.0, 90.0, true))[1].1[0], (10.0, -5.523));
    // Quarter turns from the start, then what's left.
    let e = elements(&arc(0.0, 45.0, true));
    assert_eq!((e[1].1[2], e[3].1[2], e[4].1[0]), ((0.0, -10.0), (0.0, 10.0), (2.652, 10.0)));
    // The form without clockwise: is counterclockwise.
    let p = NSBezierPath::bezierPath();
    p.moveToPoint(pt(20.0, 20.0));
    p.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle(pt(0.0, 0.0), 10.0, 0.0, 90.0);
    // After a point: a line to the arc's start.
    assert_eq!(kinds(&p), [MOVE, LINE, CURVE]);
    assert_eq!(elements(&p)[1].1, [(10.0, 0.0)]);
    // A tangent arc: a line to the first tangent point, then an arc ending
    // on the second tangent line.
    let t = NSBezierPath::bezierPath();
    t.moveToPoint(pt(0.0, 0.0));
    t.appendBezierPathWithArcFromPoint_toPoint_radius(pt(10.0, 0.0), pt(10.0, 10.0), 3.0);
    assert_eq!(kinds(&t), [MOVE, LINE, CURVE]);
    assert_eq!(elements(&t)[1].1, [(7.0, 0.0)]);
    let end = t.currentPoint();
    assert!((end.x - 10.0).abs() < 1e-9 && (end.y - 3.0).abs() < 1e-9, "{end:?}");
}

fn path_bounds(_: MainThreadMarker) {
    let p = NSBezierPath::bezierPath();
    p.moveToPoint(pt(0.0, 0.0));
    p.curveToPoint_controlPoint1_controlPoint2(pt(10.0, 0.0), pt(0.0, 10.0), pt(10.0, 10.0));
    // Tight: the curve's extent; control points: the box of all of them.
    assert_eq!(p.bounds(), rect(0.0, 0.0, 10.0, 7.5));
    assert_eq!(p.controlPointBounds(), rect(0.0, 0.0, 10.0, 10.0));
    let e = NSBezierPath::bezierPath();
    assert!(e.isEmpty());
    e.moveToPoint(pt(3.0, 4.0));
    assert!(!e.isEmpty());
    assert_eq!(e.elementCount(), 1);
    assert_eq!((e.bounds(), e.controlPointBounds()), (rect(3.0, 4.0, 0.0, 0.0), rect(3.0, 4.0, 0.0, 0.0)));
    let r = NSBezierPath::bezierPathWithRect(rect(0.0, 0.0, 1.0, 1.0));
    r.removeAllPoints();
    assert!(r.isEmpty());
    assert_eq!(r.elementCount(), 0);
}

fn hit_testing(_: MainThreadMarker) {
    let nested = NSBezierPath::bezierPathWithRect(rect(0.0, 0.0, 10.0, 10.0));
    nested.appendBezierPathWithRect(rect(2.0, 2.0, 6.0, 6.0));
    assert!(nested.containsPoint(pt(5.0, 5.0)), "the same way round: inside under non-zero");
    assert!(nested.containsPoint(pt(1.0, 1.0)));
    nested.setWindingRule(NSWindingRule::EvenOdd);
    assert!(!nested.containsPoint(pt(5.0, 5.0)), "outside under even-odd");
    assert!(nested.containsPoint(pt(1.0, 1.0)));
    let reversed = NSBezierPath::bezierPathWithRect(rect(0.0, 0.0, 10.0, 10.0));
    reversed.appendBezierPath(&NSBezierPath::bezierPathWithRect(rect(2.0, 2.0, 6.0, 6.0)).bezierPathByReversingPath());
    assert!(!reversed.containsPoint(pt(5.0, 5.0)), "opposite ways round: a hole under non-zero");
    reversed.setWindingRule(NSWindingRule::EvenOdd);
    assert!(!reversed.containsPoint(pt(5.0, 5.0)));
    assert!(!reversed.containsPoint(pt(12.0, 5.0)));
    // An oval isn't closed, but fills (and contains) as if it were.
    assert!(NSBezierPath::bezierPathWithOvalInRect(rect(0.0, 0.0, 10.0, 10.0)).containsPoint(pt(5.0, 5.0)));
    // Every edge holds its points, and so does a line.
    let square = NSBezierPath::bezierPathWithRect(rect(0.0, 0.0, 10.0, 10.0));
    for p in [pt(0.0, 5.0), pt(10.0, 5.0), pt(5.0, 0.0), pt(5.0, 10.0), pt(10.0, 10.0)] {
        assert!(square.containsPoint(p), "{p:?} on the edge");
    }
    assert!(!square.containsPoint(pt(10.5, 5.0)));
    let line = NSBezierPath::bezierPath();
    line.moveToPoint(pt(0.0, 0.0));
    line.lineToPoint(pt(10.0, 0.0));
    assert!(line.containsPoint(pt(5.0, 0.0)));
    assert!(!line.containsPoint(pt(5.0, 1.0)));
}

fn path_defaults(_: MainThreadMarker) {
    let p = NSBezierPath::bezierPath();
    assert_eq!(p.lineWidth(), 1.0);
    assert_eq!(p.lineCapStyle(), NSLineCapStyle::Butt);
    assert_eq!(p.lineJoinStyle(), NSLineJoinStyle::Miter);
    assert_eq!(p.windingRule(), NSWindingRule::NonZero);
    assert_eq!(p.miterLimit(), 10.0);
    assert!((p.flatness() - 0.6).abs() < 1e-9);
    assert_eq!(NSBezierPath::defaultLineWidth(), 1.0);
    assert_eq!(NSBezierPath::defaultMiterLimit(), 10.0);
    assert!((NSBezierPath::defaultFlatness() - 0.6).abs() < 1e-9);
    assert_eq!(NSBezierPath::defaultWindingRule(), NSWindingRule::NonZero);
    assert_eq!(NSBezierPath::defaultLineCapStyle(), NSLineCapStyle::Butt);
    assert_eq!(NSBezierPath::defaultLineJoinStyle(), NSLineJoinStyle::Miter);
    NSBezierPath::setDefaultLineWidth(3.0);
    let later = NSBezierPath::bezierPath();
    assert_eq!((later.lineWidth(), p.lineWidth()), (3.0, 1.0), "new paths take the default; made ones keep theirs");
    // The class drawing methods use it too.
    let rep = bitmap(20, 20);
    draw_in(&rep, |_| {
        NSColor::blackColor().setStroke();
        NSBezierPath::strokeLineFromPoint_toPoint(pt(0.0, 10.0), pt(20.0, 10.0));
    });
    NSBezierPath::setDefaultLineWidth(1.0);
    let rows = (0..20).filter(|&y| pixel(&rep, 10, y)[3] > 0).count();
    assert!((3..=4).contains(&rows), "three points wide ({rows} rows)");

    let d = NSBezierPath::bezierPath();
    let pattern = [4.0, 2.0, 1.0];
    // SAFETY: three lengths.
    unsafe { d.setLineDash_count_phase(pattern.as_ptr(), 3, 1.5) };
    let (mut count, mut phase) = (0isize, 0.0);
    // SAFETY: a NULL pattern asks only for the count.
    unsafe { d.getLineDash_count_phase(std::ptr::null_mut(), &mut count, &mut phase) };
    assert_eq!((count, phase), (3, 1.5));
    let mut back = [0.0; 3];
    // SAFETY: room for three lengths.
    unsafe { d.getLineDash_count_phase(back.as_mut_ptr(), &mut count, &mut phase) };
    assert_eq!(back, pattern);
    // No lengths keep the phase, unless there's no pattern at all.
    // SAFETY: no lengths.
    unsafe { d.setLineDash_count_phase(pattern.as_ptr(), 0, 3.0) };
    // SAFETY: a NULL pattern asks only for the count.
    unsafe { d.getLineDash_count_phase(std::ptr::null_mut(), &mut count, &mut phase) };
    assert_eq!((count, phase), (0, 3.0));
    // SAFETY: no pattern.
    unsafe { d.setLineDash_count_phase(std::ptr::null(), 0, 3.0) };
    // SAFETY: a NULL pattern asks only for the count.
    unsafe { d.getLineDash_count_phase(std::ptr::null_mut(), &mut count, &mut phase) };
    assert_eq!((count, phase), (0, 0.0));
    // Settings are kept as given, even ones drawing can't use.
    d.setMiterLimit(0.5);
    assert_eq!(d.miterLimit(), 0.5);
    NSBezierPath::setDefaultMiterLimit(0.5);
    assert_eq!(NSBezierPath::defaultMiterLimit(), 0.5);
    NSBezierPath::setDefaultMiterLimit(10.0);
}

/// The alpha of each row at column `x` of a `size` × `size` (points) bitmap
/// at `scale` pixels a point, after `f` draws.
fn column(size: f64, scale: f64, x: f64, f: impl FnOnce()) -> Vec<u8> {
    let n = (size * scale) as isize;
    let rep = bitmap(n, n);
    rep.setSize(NSSize::new(size, size));
    draw_in(&rep, |_| f());
    (0..n).map(|y| pixel(&rep, (x * scale) as isize, y)[3]).collect()
}

fn line(y: f64, width: f64) -> impl FnOnce() {
    move || {
        let p = NSBezierPath::bezierPath();
        p.moveToPoint(pt(0.0, y));
        p.lineToPoint(pt(10.0, y));
        p.setLineWidth(width);
        NSColor::blackColor().setStroke();
        p.stroke();
    }
}

fn stroke_pixels(_: MainThreadMarker) {
    let rows = |c: &[u8]| c.iter().filter(|&&a| a > 0).count();
    // A 1-point line on a half pixel covers one row, whole.
    let c = column(10.0, 1.0, 5.0, line(5.5, 1.0));
    assert_eq!((rows(&c), c[4]), (1, 255));
    // On a whole pixel it straddles two, partly.
    let c = column(10.0, 1.0, 5.0, line(5.0, 1.0));
    assert!(rows(&c) == 2 && partial([0, 0, 0, c[4]]) && partial([0, 0, 0, c[5]]), "{c:?}");
    // Width 0 is one device pixel, at 1× and at 2×.
    let c = column(10.0, 1.0, 5.0, line(5.5, 0.0));
    assert_eq!(rows(&c), 1);
    let c = column(10.0, 2.0, 5.0, line(5.25, 0.0));
    assert_eq!(rows(&c), 1, "{c:?}");

    // Caps: square extends by half the width, butt doesn't.
    let cap = |style: NSLineCapStyle| {
        let rep = bitmap(20, 10);
        draw_in(&rep, |_| {
            let p = NSBezierPath::bezierPath();
            p.moveToPoint(pt(5.0, 5.0));
            p.lineToPoint(pt(15.0, 5.0));
            p.setLineWidth(4.0);
            p.setLineCapStyle(style);
            NSColor::blackColor().setStroke();
            p.stroke();
        });
        (0..20).filter(|&x| pixel(&rep, x, 4)[3] > 128).collect::<Vec<_>>()
    };
    assert_eq!(cap(NSLineCapStyle::Butt), (5..15).collect::<Vec<_>>());
    assert_eq!(cap(NSLineCapStyle::Square), (3..17).collect::<Vec<_>>());

    // Joins at a sharp corner: round fills it, miter and bevel differ.
    let join = |style: NSLineJoinStyle| {
        let rep = bitmap(40, 40);
        draw_in(&rep, |_| {
            let p = NSBezierPath::bezierPath();
            p.moveToPoint(pt(5.0, 5.0));
            p.lineToPoint(pt(20.0, 30.0));
            p.lineToPoint(pt(35.0, 5.0));
            p.setLineWidth(6.0);
            p.setLineJoinStyle(style);
            NSColor::blackColor().setStroke();
            p.stroke();
        });
        // Pixels above the corner (the join's side), top-left origin.
        (0..10).map(|y| pixel(&rep, 20, y)[3] as u32).sum::<u32>()
    };
    let (miter, round, bevel) =
        (join(NSLineJoinStyle::Miter), join(NSLineJoinStyle::Round), join(NSLineJoinStyle::Bevel));
    assert!(miter > round && round > bevel, "miter {miter} round {round} bevel {bevel}");

    // A (4, 4) dash leaves the gaps' middles untouched.
    let rep = bitmap(20, 4);
    draw_in(&rep, |_| {
        let p = NSBezierPath::bezierPath();
        p.moveToPoint(pt(0.0, 2.0));
        p.lineToPoint(pt(20.0, 2.0));
        p.setLineWidth(2.0);
        let dash = [4.0, 4.0];
        // SAFETY: two lengths.
        unsafe { p.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
        NSColor::blackColor().setStroke();
        p.stroke();
    });
    for x in [2, 10, 18] {
        assert_eq!(pixel(&rep, x, 1)[3], 255, "dash at {x}");
    }
    for x in [6, 14] {
        assert_eq!(pixel(&rep, x, 1)[3], 0, "gap at {x}");
    }
}

fn derived_paths(_: MainThreadMarker) {
    let p = NSBezierPath::bezierPath();
    p.moveToPoint(pt(0.0, 0.0));
    p.lineToPoint(pt(5.0, 0.0));
    p.curveToPoint_controlPoint1_controlPoint2(pt(10.0, 5.0), pt(7.0, 0.0), pt(10.0, 3.0));
    let r = p.bezierPathByReversingPath();
    assert_eq!(r.bounds(), p.bounds());
    assert_eq!(r.currentPoint(), pt(0.0, 0.0), "ends where the original began");
    assert_eq!(elements(&r)[0], (MOVE, vec![(10.0, 5.0)]));
    let rr = NSBezierPath::bezierPathWithRect(rect(0.0, 0.0, 2.0, 2.0)).bezierPathByReversingPath();
    assert_eq!(
        elements(&rr)[..2],
        [(MOVE, vec![(0.0, 0.0)]), (LINE, vec![(0.0, 2.0)])],
        "closed: same start, other way"
    );

    let oval = NSBezierPath::bezierPathWithOvalInRect(rect(0.0, 0.0, 100.0, 100.0));
    let flat = oval.bezierPathByFlatteningPath();
    assert!(!kinds(&flat).contains(&CURVE) && flat.elementCount() > 8);
    let (a, b) = (oval.bounds(), flat.bounds());
    let within = |x: f64, y: f64| (x - y).abs() <= oval.flatness() + 1e-9;
    assert!(
        within(a.origin.x, b.origin.x) && within(a.size.width, b.size.width) && within(a.size.height, b.size.height)
    );
}

fn clip_paths(_: MainThreadMarker) {
    let rep = bitmap(10, 10);
    draw_in(&rep, |_| {
        NSBezierPath::bezierPathWithRect(rect(0.0, 0.0, 5.0, 10.0)).addClip();
        NSRectClip(rect(0.0, 0.0, 3.0, 10.0));
        // setClip replaces the clip, wider again.
        NSBezierPath::bezierPathWithRect(rect(0.0, 0.0, 8.0, 10.0)).setClip();
        NSColor::blackColor().setFill();
        NSRectFill(rect(0.0, 0.0, 10.0, 10.0));
    });
    assert_px(&rep, 2, 5, BLACK);
    assert_px(&rep, 6, 5, BLACK);
    assert_px(&rep, 9, 5, CLEAR);
    // A round clip: the corners stay as they were, even for NSRectFill,
    // which copies.
    let rep = bitmap(10, 10);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| {
        NSBezierPath::bezierPathWithOvalInRect(rect(0.0, 0.0, 10.0, 10.0)).addClip();
        NSColor::redColor().setFill();
        NSRectFill(rect(0.0, 0.0, 10.0, 10.0));
    });
    assert_px(&rep, 5, 5, RED);
    assert_px(&rep, 0, 0, BLUE);
    assert_px(&rep, 9, 9, BLUE);
}

fn fills_and_strokes(_: MainThreadMarker) {
    let rep = bitmap(10, 10);
    draw_in(&rep, |_| {
        NSColor::redColor().setFill();
        NSColor::blueColor().setStroke();
        let p = NSBezierPath::bezierPathWithRect(rect(2.5, 2.5, 5.0, 5.0));
        p.fill();
        p.stroke();
    });
    assert_px(&rep, 5, 5, RED);
    assert_px(&rep, 2, 5, BLUE);
    assert_px(&rep, 0, 0, CLEAR);
    // -set sets both.
    let rep = bitmap(4, 4);
    draw_in(&rep, |_| {
        NSColor::greenColor().set();
        NSBezierPath::bezierPathWithRect(rect(0.5, 0.5, 3.0, 3.0)).stroke();
    });
    assert_px(&rep, 0, 1, GREEN);
}

/// Pixels of `rep` with ink, farther than `margin` pixels outside the
/// circle `(cx, cy, r)` (pixels, top-left origin).
fn ink_outside(rep: &NSBitmapImageRep, (cx, cy, r): (f64, f64, f64), margin: f64) -> usize {
    let mut n = 0;
    for y in 0..rep.pixelsHigh() {
        for x in 0..rep.pixelsWide() {
            let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
            if d > r + margin && pixel(rep, x, y)[3] > 0 {
                n += 1;
            }
        }
    }
    n
}

fn clipped_text(_: MainThreadMarker) {
    // Text drawn across a bitmap, then again inside a round clip: none of
    // it lands outside the circle.
    let text = NSString::from_str("WWWWWWWW WWWWWWWW WWWWWWWW WWWWWWWW WWWWWWWW WWWWWWWW");
    let draw = |clip: bool| {
        let rep = bitmap(60, 60);
        draw_in(&rep, |_| {
            if clip {
                NSBezierPath::bezierPathWithOvalInRect(rect(10.0, 10.0, 40.0, 40.0)).addClip();
            }
            let font = NSFont::boldSystemFontOfSize(24.0);
            // SAFETY: a constant key.
            let key = unsafe { NSFontAttributeName };
            let attrs = NSDictionary::from_slices(&[key], &[&*font as &AnyObject]);
            // SAFETY: the attributes hold a font.
            unsafe { text.drawInRect_withAttributes(rect(0.0, 0.0, 60.0, 60.0), Some(&attrs)) };
        });
        rep
    };
    let circle = (30.0, 30.0, 20.0);
    assert!(ink_outside(&draw(false), circle, 1.5) > 0, "the text reaches outside the circle");
    assert_eq!(ink_outside(&draw(true), circle, 1.5), 0, "clipped to it");
}

// Gradients.

fn srgb(r: f64, g: f64, b: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
}

/// sRGB red, green, blue and alpha of `c`.
fn components(c: &NSColor) -> [f64; 4] {
    let c = c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()).expect("an sRGB color");
    [c.redComponent(), c.greenComponent(), c.blueComponent(), c.alphaComponent()]
}

fn about(a: [f64; 4], b: [f64; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01)
}

/// A red-to-blue gradient in sRGB.
fn red_to_blue() -> Retained<NSGradient> {
    NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &srgb(1.0, 0.0, 0.0), &srgb(0.0, 0.0, 1.0))
        .expect("a gradient")
}

/// Mostly the start color (red), mostly the end color (blue), or between.
fn reddish(p: [u8; 4]) -> bool {
    p[3] > 200 && p[0] > 180 && p[2] < 75
}

fn bluish(p: [u8; 4]) -> bool {
    p[3] > 200 && p[2] > 180 && p[0] < 75
}

fn between(p: [u8; 4]) -> bool {
    p[3] > 200 && (90..170).contains(&p[0]) && (90..170).contains(&p[2])
}

fn gradient_colors(_: MainThreadMarker) {
    let g = red_to_blue();
    assert_eq!(g.numberOfColorStops(), 2);
    assert_eq!(g.colorSpace().colorSpaceModel(), NSColorSpaceModel::RGB);
    let stop = |g: &NSGradient, i: isize| {
        let mut color = NSColor::blackColor();
        let mut at = -1.0;
        // SAFETY: both out-parameters are writable.
        unsafe { g.getColor_location_atIndex(Some(&mut color), &mut at, i) };
        (components(&color), at)
    };
    assert_eq!(stop(&g, 0), ([1.0, 0.0, 0.0, 1.0], 0.0));
    assert_eq!(stop(&g, 1), ([0.0, 0.0, 1.0, 1.0], 1.0));
    // Interpolated in sRGB, clamped at the ends.
    assert!(about(components(&g.interpolatedColorAtLocation(0.5)), [0.5, 0.0, 0.5, 1.0]));
    assert!(about(components(&g.interpolatedColorAtLocation(-1.0)), [1.0, 0.0, 0.0, 1.0]));
    assert!(about(components(&g.interpolatedColorAtLocation(2.0)), [0.0, 0.0, 1.0, 1.0]));
    // Colors alone are spread evenly; one color makes two stops.
    let three = NSArray::from_retained_slice(&[srgb(1.0, 0.0, 0.0), srgb(0.0, 1.0, 0.0), srgb(0.0, 0.0, 1.0)]);
    let g = NSGradient::initWithColors(NSGradient::alloc(), &three).expect("a gradient");
    assert_eq!((0..3).map(|i| stop(&g, i).1).collect::<Vec<_>>(), [0.0, 0.5, 1.0]);
    let one = NSGradient::initWithColors(NSGradient::alloc(), &NSArray::from_retained_slice(&[srgb(1.0, 0.0, 0.0)]))
        .expect("a gradient");
    assert_eq!(one.numberOfColorStops(), 2);
    // Locations are sorted; beyond the last stop is the last color.
    let two = NSArray::from_retained_slice(&[srgb(1.0, 0.0, 0.0), srgb(0.0, 0.0, 1.0)]);
    let at = [0.8, 0.2];
    // SAFETY: a location for each color.
    let g = unsafe {
        NSGradient::initWithColors_atLocations_colorSpace(
            NSGradient::alloc(),
            &two,
            at.as_ptr(),
            &NSColorSpace::sRGBColorSpace(),
        )
    }
    .expect("a gradient");
    assert_eq!(stop(&g, 0), ([0.0, 0.0, 1.0, 1.0], 0.2));
    assert!(about(components(&g.interpolatedColorAtLocation(0.5)), [0.5, 0.0, 0.5, 1.0]));
    assert!(about(components(&g.interpolatedColorAtLocation(0.9)), [1.0, 0.0, 0.0, 1.0]));
}

fn gradient_drawing(mtm: MainThreadMarker) {
    let g = red_to_blue();
    // Across a rectangle at an angle: 0 from the left, 90 from the bottom.
    let rep = bitmap(8, 8);
    draw_in(&rep, |_| g.drawInRect_angle(rect(0.0, 0.0, 8.0, 8.0), 0.0));
    assert!(reddish(pixel(&rep, 0, 4)) && bluish(pixel(&rep, 7, 4)) && between(pixel(&rep, 3, 4)));
    let rep = bitmap(8, 8);
    draw_in(&rep, |_| g.drawInRect_angle(rect(0.0, 0.0, 8.0, 8.0), 90.0));
    assert!(reddish(pixel(&rep, 4, 7)) && bluish(pixel(&rep, 4, 0)));
    let rep = bitmap(8, 8);
    draw_in(&rep, |_| g.drawInRect_angle(rect(0.0, 0.0, 8.0, 8.0), 180.0));
    assert!(bluish(pixel(&rep, 0, 4)) && reddish(pixel(&rep, 7, 4)));
    // In a flipped view, 90 degrees runs from the top.
    let flipped_g = g.clone();
    let view = draw_view(mtm, rect(0.0, 0.0, 8.0, 8.0), true, move |_, _| {
        flipped_g.drawInRect_angle(rect(0.0, 0.0, 8.0, 8.0), 90.0)
    });
    let rep = snapshot(&view, 1.0);
    assert!(reddish(pixel(&rep, 4, 0)) && bluish(pixel(&rep, 4, 7)));

    // From a point to a point: only between them, unless extended.
    for (options, before, after) in [
        (NSGradientDrawingOptions::empty(), false, false),
        (NSGradientDrawingOptions::DrawsBeforeStartingLocation, true, false),
        (NSGradientDrawingOptions::DrawsAfterEndingLocation, false, true),
    ] {
        let rep = bitmap(12, 2);
        draw_in(&rep, |_| g.drawFromPoint_toPoint_options(pt(4.0, 0.0), pt(8.0, 0.0), options));
        assert!(reddish(pixel(&rep, 4, 0)) && bluish(pixel(&rep, 7, 0)), "{options:?}");
        assert_eq!(reddish(pixel(&rep, 1, 0)), before, "{options:?}");
        assert_eq!(pixel(&rep, 1, 0) == CLEAR, !before, "{options:?}");
        assert_eq!(bluish(pixel(&rep, 10, 0)), after, "{options:?}");
        assert_eq!(pixel(&rep, 10, 0) == CLEAR, !after, "{options:?}");
    }
    // Between two circles: inside the first and outside the second only
    // when extended.
    let both =
        NSGradientDrawingOptions::DrawsBeforeStartingLocation | NSGradientDrawingOptions::DrawsAfterEndingLocation;
    for options in [NSGradientDrawingOptions::empty(), both] {
        let rep = bitmap(12, 12);
        draw_in(&rep, |_| {
            g.drawFromCenter_radius_toCenter_radius_options(pt(6.0, 6.0), 2.0, pt(6.0, 6.0), 5.0, options)
        });
        let extended = options == both;
        assert_eq!(reddish(pixel(&rep, 5, 5)), extended, "{options:?}");
        assert_eq!(pixel(&rep, 5, 5) == CLEAR, !extended, "{options:?}");
        assert_eq!(pixel(&rep, 0, 0) == CLEAR, !extended, "{options:?}");
        assert!(reddish(pixel(&rep, 3, 6)) || between(pixel(&rep, 3, 6)), "{options:?}");
        assert!(bluish(pixel(&rep, 1, 6)) || between(pixel(&rep, 1, 6)), "{options:?}");
    }
    // Radial in a rectangle: the start at the center (or where asked), the
    // end at the far corner.
    let rep = bitmap(10, 10);
    draw_in(&rep, |_| g.drawInRect_relativeCenterPosition(rect(0.0, 0.0, 10.0, 10.0), NSPoint::ZERO));
    assert!(reddish(pixel(&rep, 5, 5)) && bluish(pixel(&rep, 0, 0)) && bluish(pixel(&rep, 9, 9)));
    let rep = bitmap(10, 10);
    draw_in(&rep, |_| g.drawInRect_relativeCenterPosition(rect(0.0, 0.0, 10.0, 10.0), pt(1.0, 0.0)));
    assert!(reddish(pixel(&rep, 9, 5)) && bluish(pixel(&rep, 0, 5)));
    // In a path: its bounds set the ends, its shape the pixels.
    let rep = bitmap(10, 10);
    draw_in(&rep, |_| {
        g.drawInBezierPath_angle(&NSBezierPath::bezierPathWithOvalInRect(rect(0.0, 0.0, 10.0, 10.0)), 0.0)
    });
    assert_eq!(pixel(&rep, 0, 0), CLEAR);
    assert!(between(pixel(&rep, 4, 5)) || between(pixel(&rep, 5, 5)));
    let rep = bitmap(10, 2);
    draw_in(&rep, |_| g.drawInBezierPath_angle(&NSBezierPath::bezierPathWithRect(rect(2.0, 0.0, 6.0, 2.0)), 0.0));
    assert_eq!((pixel(&rep, 1, 0), pixel(&rep, 8, 0)), (CLEAR, CLEAR));
    assert!(reddish(pixel(&rep, 2, 0)) && bluish(pixel(&rep, 7, 0)));
}

// Shadows.

fn shadows(mtm: MainThreadMarker) {
    let s = NSShadow::new();
    assert_eq!((s.shadowOffset(), s.shadowBlurRadius()), (NSSize::ZERO, 0.0));
    let color = components(&s.shadowColor().expect("a color"));
    assert!(about(color, [0.0, 0.0, 0.0, 1.0 / 3.0]), "black at a third: {color:?}");
    s.setShadowColor(None);
    assert!(s.shadowColor().is_none());
    s.setShadowBlurRadius(-3.0);
    assert_eq!(s.shadowBlurRadius(), -3.0, "kept as given");

    // The offset is in base coordinates: a positive height goes up, in a
    // flipped view too.
    for flipped in [false, true] {
        let view = draw_view(mtm, rect(0.0, 0.0, 20.0, 20.0), flipped, |_, _| {
            let s = NSShadow::new();
            s.setShadowOffset(NSSize::new(4.0, 4.0));
            s.setShadowColor(Some(&NSColor::blackColor()));
            s.set();
            NSColor::redColor().setFill();
            NSRectFill(rect(6.0, 6.0, 8.0, 8.0));
        });
        let rep = snapshot(&view, 1.0);
        assert_px(&rep, 8, 12, RED);
        assert_px(&rep, 16, 3, BLACK);
        assert_px(&rep, 8, 3, CLEAR);
        assert_px(&rep, 16, 12, CLEAR);
    }
    // In points: twice the pixels at twice the scale.
    let rep = bitmap(40, 40);
    rep.setSize(NSSize::new(20.0, 20.0));
    draw_in(&rep, |_| {
        let s = NSShadow::new();
        s.setShadowOffset(NSSize::new(4.0, -4.0));
        s.setShadowColor(Some(&NSColor::blackColor()));
        s.set();
        NSColor::redColor().setFill();
        NSRectFill(rect(6.0, 6.0, 8.0, 8.0));
    });
    assert_px(&rep, 32, 32, BLACK);
    assert_px(&rep, 32, 37, CLEAR);
    // Blurred by about its radius, and no further.
    let view = draw_view(mtm, rect(0.0, 0.0, 30.0, 30.0), false, |_, _| {
        let s = NSShadow::new();
        s.setShadowBlurRadius(4.0);
        s.setShadowColor(Some(&NSColor::blackColor()));
        s.set();
        NSColor::redColor().setFill();
        NSBezierPath::fillRect(rect(10.0, 10.0, 10.0, 10.0));
    });
    let rep = snapshot(&view, 1.0);
    assert!(partial(pixel(&rep, 9, 15)), "{:?}", pixel(&rep, 9, 15));
    assert_px(&rep, 4, 15, CLEAR);
    assert_px(&rep, 15, 15, RED);
    // Restoring the graphics state takes it away.
    let rep = bitmap(20, 20);
    draw_in(&rep, |ctx| {
        ctx.saveGraphicsState();
        let s = NSShadow::new();
        s.setShadowOffset(NSSize::new(4.0, -4.0));
        s.setShadowColor(Some(&NSColor::blackColor()));
        s.set();
        ctx.restoreGraphicsState();
        NSColor::redColor().setFill();
        NSRectFill(rect(6.0, 6.0, 8.0, 8.0));
    });
    assert_px(&rep, 16, 16, CLEAR);
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("context_lifetime", context_lifetime),
        ("bitmap_contexts", bitmap_contexts),
        ("fresh_state_per_view", fresh_state_per_view),
        ("save_and_restore", save_and_restore),
        ("rect_fill_copies", rect_fill_copies),
        ("antialiasing", antialiasing),
        ("frames_erasing_and_clips", frames_erasing_and_clips),
        ("fill_lists", fill_lists),
        ("copies", copies),
        ("transform_math", transform_math),
        ("transforms_move_drawing", transforms_move_drawing),
        ("rects_being_drawn", rects_being_drawn),
        ("view_snapshots", view_snapshots),
        ("path_structure", path_structure),
        ("close_path", close_path),
        ("arcs", arcs),
        ("path_bounds", path_bounds),
        ("hit_testing", hit_testing),
        ("path_defaults", path_defaults),
        ("stroke_pixels", stroke_pixels),
        ("derived_paths", derived_paths),
        ("clip_paths", clip_paths),
        ("clipped_text", clipped_text),
        ("fills_and_strokes", fills_and_strokes),
        ("gradient_colors", gradient_colors),
        ("gradient_drawing", gradient_drawing),
        ("shadows", shadows),
    ];
    let _ = Cell::new(0);
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
