//! Images, checked on macOS and on Linux alike: NSImage and its
//! representations, decoding the files in `fixtures/` (made by
//! `scripts/make-image-fixtures`, never with Apple's tools), drawing them
//! and the representation drawing picks, drawing handlers, bitmap layout
//! and encoding. Pixels are read back from bitmaps as in `drawing.rs`.
//!
//! Parts that need `NSData` from a file run where Foundation has the
//! class; they say so and are skipped where it hasn't yet.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.
// Flipped images and lockFocus are deprecated, and pinned all the same.
#![allow(deprecated)]

mod common;

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use common::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{AnyThread, ClassType, MainThreadMarker, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBitmapFormat, NSBitmapImageFileType,
    NSBitmapImageRep, NSColor, NSCompositingOperation, NSCustomImageRep, NSDeviceRGBColorSpace, NSGraphicsContext,
    NSImage, NSImageCacheMode, NSImageColorSyncProfileData, NSImageCompressionFactor, NSImageCompressionMethod,
    NSImageCurrentFrame, NSImageCurrentFrameDuration, NSImageDitherTransparency, NSImageEXIFData,
    NSImageFallbackBackgroundColor, NSImageFrameCount, NSImageGamma, NSImageHintCTM, NSImageHintInterpolation,
    NSImageHintUserInterfaceLayoutDirection, NSImageIPTCData, NSImageInterlaced, NSImageLoopCount, NSImageProgressive,
    NSImageRGBColorTable, NSImageRep, NSImageResizingMode, NSImageSymbolConfiguration, NSImageSymbolScale, NSRectFill,
};
use objc2_foundation::{NSArray, NSCopying, NSData, NSDictionary, NSNumber, NSRect, NSSize, NSString};

use sidestep as _;

fn fixture(name: &str) -> Retained<NSString> {
    NSString::from_str(&format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
}

fn from_file(name: &str) -> Retained<NSImage> {
    NSImage::initWithContentsOfFile(NSImage::alloc(), &fixture(name)).unwrap_or_else(|| panic!("{name} loads"))
}

/// A fixture's bytes as an `NSData`, where Foundation has the class.
fn data(name: &str) -> Option<Retained<NSData>> {
    let class = AnyClass::get(c"NSData")?;
    // SAFETY: +dataWithContentsOfFile: takes a path and returns an NSData
    // or nil.
    unsafe { msg_send![class, dataWithContentsOfFile: &*fixture(name)] }
}

fn has_data() -> bool {
    let has = AnyClass::get(c"NSData").is_some();
    if !has {
        println!("    (skipping the parts that need NSData, which Foundation doesn't have yet)");
    }
    has
}

/// The only representation of `image`, a bitmap.
fn only_bitmap(image: &NSImage) -> Retained<NSBitmapImageRep> {
    let reps = image.representations();
    assert_eq!(reps.count(), 1, "one representation");
    reps.objectAtIndex(0).downcast::<NSBitmapImageRep>().expect("a bitmap")
}

/// The bytes of pixel (x, y) of a rep four bytes a pixel.
fn bytes_at(rep: &NSBitmapImageRep, x: isize, y: isize) -> [u8; 4] {
    let at = y as usize * rep.bytesPerRow() as usize + x as usize * 4;
    // SAFETY: inside the bitmap.
    unsafe { std::ptr::read(rep.bitmapData().add(at).cast::<[u8; 4]>()) }
}

/// Whether each channel of `a` is within `tolerance` of `b`'s.
fn close(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
    a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= tolerance)
}

fn basics(_: MainThreadMarker) {
    let sized = NSImage::initWithSize(NSImage::alloc(), NSSize::new(10.0, 5.0));
    assert_eq!(sized.representations().count(), 0, "no representations");
    assert!(sized.isValid(), "a size is enough to be valid");
    assert_eq!(sized.size(), NSSize::new(10.0, 5.0));
    let empty = NSImage::new();
    assert_eq!(empty.size(), NSSize::ZERO);
    assert!(!empty.isValid());
    // Defaults.
    assert!(!sized.isTemplate() && !sized.isFlipped());
    assert_eq!(sized.cacheMode(), NSImageCacheMode::Default);
    assert_eq!(sized.alignmentRect(), rect(0.0, 0.0, 10.0, 5.0));
    let insets = sized.capInsets();
    assert_eq!((insets.top, insets.left, insets.bottom, insets.right), (0.0, 0.0, 0.0, 0.0));
    // (The default resizing mode's raw value is 1 on every Mac, which
    // objc2 calls Tile on arm64 and Stretch on x86_64: not asserted.)
    sized.setResizingMode(NSImageResizingMode::Tile);
    assert_eq!(sized.resizingMode(), NSImageResizingMode::Tile);
    assert_eq!(sized.backgroundColor().alphaComponent(), 0.0, "clear");
    assert!(sized.accessibilityDescription().is_none());

    // Names: one image per name; nil unregisters.
    let name = NSString::from_str("conformance-image");
    assert!(sized.name().is_none());
    assert!(sized.setName(Some(&name)));
    assert_eq!(sized.name().map(|n| n.to_string()).as_deref(), Some("conformance-image"));
    let found = NSImage::imageNamed(&name).expect("registered");
    assert!(std::ptr::eq(&*found, &*sized), "the same image");
    let other = NSImage::initWithSize(NSImage::alloc(), NSSize::new(1.0, 1.0));
    assert!(!other.setName(Some(&name)), "the name is taken");
    assert!(sized.setName(None));
    assert!(NSImage::imageNamed(&name).is_none());
    assert!(NSImage::imageNamed(&NSString::from_str("no-such-image-anywhere")).is_none());

    // setSize: changes how large the image is, not its representations.
    let image = from_file("halves.png");
    image.setSize(NSSize::new(8.0, 8.0));
    assert_eq!(image.size(), NSSize::new(8.0, 8.0));
    assert_eq!(only_bitmap(&image).size(), NSSize::new(4.0, 4.0));
    let rep = bitmap(8, 8);
    draw_in(&rep, |_| {
        image.drawAtPoint_fromRect_operation_fraction(
            pt(0.0, 0.0),
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            1.0,
        )
    });
    assert_px(&rep, 7, 0, RED);
    assert_px(&rep, 7, 7, BLUE);
    // A copy is an image of the same size.
    // SAFETY: -copy returns a new image.
    let copy: Retained<NSImage> = unsafe { msg_send![&*image, copy] };
    assert_eq!(copy.size(), NSSize::new(8.0, 8.0));
    // An image with no size of its own takes its first representation's
    // when it's added, and keeps it.
    let image = NSImage::new();
    let rep = bitmap(2, 2);
    image.addRepresentation(&rep);
    assert_eq!(image.size(), NSSize::new(2.0, 2.0));
    rep.setSize(NSSize::new(5.0, 5.0));
    assert_eq!(image.size(), NSSize::new(2.0, 2.0));

    assert!(NSImage::imageTypes().containsObject(&NSString::from_str("public.png")));
    // SAFETY: +imageTypes takes nothing and returns an array of strings.
    let bitmap_types: Retained<NSArray<NSString>> = unsafe { msg_send![NSBitmapImageRep::class(), imageTypes] };
    assert!(bitmap_types.containsObject(&NSString::from_str("public.jpeg")));
}

fn decoding(_: MainThreadMarker) {
    // PNG: its density sets its size in points.
    for (name, points) in [("rgba-72dpi.png", NSSize::new(4.0, 2.0)), ("rgba-144dpi.png", NSSize::new(2.0, 1.0))] {
        let image = from_file(name);
        assert_eq!(image.size(), points, "{name}");
        let rep = only_bitmap(&image);
        assert_eq!(rep.size(), points);
        assert_eq!((rep.pixelsWide(), rep.pixelsHigh()), (4, 2));
        assert_eq!((rep.bitsPerSample(), rep.samplesPerPixel(), rep.bitsPerPixel(), rep.bytesPerRow()), (8, 4, 32, 16));
        assert_eq!(rep.bitmapFormat(), NSBitmapFormat::AlphaNonpremultiplied);
        assert!(rep.hasAlpha() && !rep.isOpaque() && !rep.isPlanar());
        assert_eq!(rep.colorSpaceName().to_string(), "NSCalibratedRGBColorSpace");
        // Row 0 is the top; alpha isn't premultiplied.
        assert_eq!(bytes_at(&rep, 0, 0), [255, 0, 0, 255]);
        assert_eq!(bytes_at(&rep, 2, 0), [0, 0, 255, 255]);
        assert_eq!(bytes_at(&rep, 0, 1), [255, 0, 0, 128]);
        assert_eq!(bytes_at(&rep, 3, 1), [192, 192, 192, 255]);
    }
    // GIF (its first frame) and WebP, 3 × 2.
    for (name, alpha) in [("small.gif", false), ("small.webp", true)] {
        let rep = only_bitmap(&from_file(name));
        assert_eq!(
            (rep.pixelsWide(), rep.pixelsHigh(), rep.bitsPerPixel(), rep.bytesPerRow()),
            (3, 2, 32, 12),
            "{name}"
        );
        assert_eq!((rep.hasAlpha(), rep.samplesPerPixel()), (alpha, if alpha { 4 } else { 3 }), "{name}");
        let rgb = |p: [u8; 4]| [p[0], p[1], p[2], 255];
        assert_eq!(rgb(bytes_at(&rep, 0, 0)), RED, "{name}");
        assert_eq!(rgb(bytes_at(&rep, 1, 0)), GREEN, "{name}");
        assert_eq!(rgb(bytes_at(&rep, 0, 1)), GREEN, "{name}");
        assert_eq!(rgb(bytes_at(&rep, 2, 1)), RED, "{name}");
    }
    // A JPEG stored sideways comes upright: 4 × 2 becomes 2 × 4, red on
    // top.
    let image = from_file("orientation-6.jpg");
    assert_eq!(image.size(), NSSize::new(2.0, 4.0));
    let rep = only_bitmap(&image);
    assert_eq!((rep.pixelsWide(), rep.pixelsHigh()), (2, 4));
    assert!(close(bytes_at(&rep, 0, 0), RED, 6) && close(bytes_at(&rep, 1, 3), BLUE, 6));

    // Files that aren't images.
    assert!(NSImage::initWithContentsOfFile(NSImage::alloc(), &fixture("garbage.png")).is_none());
    assert!(NSImageRep::imageRepWithContentsOfFile(&fixture("garbage.png")).is_none());
    // Nor is a file claiming more pixels than memory holds.
    assert!(NSImageRep::imageRepWithContentsOfFile(&fixture("huge.tiff")).is_none());
    assert!(NSImage::initWithContentsOfFile(NSImage::alloc(), &fixture("huge.tiff")).is_none());
    // By reference, an image exists, but isn't valid.
    let missing =
        NSImage::initByReferencingFile(NSImage::alloc(), &NSString::from_str("/no/such/file.png")).expect("an image");
    assert!(!missing.isValid() && missing.representations().count() == 0);
    let referenced = NSImage::initByReferencingFile(NSImage::alloc(), &fixture("halves.png")).expect("an image");
    assert_eq!(referenced.size(), NSSize::new(4.0, 4.0));

    if !has_data() {
        return;
    }
    let png = data("halves.png").expect("the file");
    let image = NSImage::initWithData(NSImage::alloc(), &png).expect("a PNG");
    assert_eq!(image.size(), NSSize::new(4.0, 4.0));
    let jpeg = data("orientation-6.jpg").expect("the file");
    assert_eq!(NSImage::initWithData(NSImage::alloc(), &jpeg).unwrap().size(), NSSize::new(2.0, 4.0));
    let sideways = NSImage::initWithDataIgnoringOrientation(NSImage::alloc(), &jpeg).unwrap();
    assert_eq!(sideways.size(), NSSize::new(4.0, 2.0), "as stored");
    let rep = only_bitmap(&sideways);
    assert_eq!((rep.samplesPerPixel(), rep.hasAlpha(), rep.bitsPerPixel()), (3, false, 32));
    assert!(close(bytes_at(&rep, 0, 0), RED, 6) && close(bytes_at(&rep, 3, 0), BLUE, 6));
    let garbage = data("garbage.png").expect("the file");
    assert!(NSBitmapImageRep::imageRepWithData(&garbage).is_none());
    let image = NSImage::initWithData(NSImage::alloc(), &garbage);
    assert!(image.is_none_or(|i| !i.isValid() && i.representations().count() == 0), "nothing to draw");
}

fn draw_orientation(mtm: MainThreadMarker) {
    // Red on top, blue below, drawn three ways into a flipped and an
    // unflipped view.
    let halves = from_file("halves.png");
    for flipped in [false, true] {
        let image = halves.clone();
        let view = draw_view(mtm, rect(0.0, 0.0, 16.0, 4.0), flipped, move |_, _| {
            image.drawInRect(rect(0.0, 0.0, 4.0, 4.0));
            image.drawInRect_fromRect_operation_fraction(
                rect(4.0, 0.0, 4.0, 4.0),
                NSRect::ZERO,
                NSCompositingOperation::SourceOver,
                1.0,
            );
            // SAFETY: no hints.
            unsafe {
                image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                    rect(8.0, 0.0, 4.0, 4.0),
                    NSRect::ZERO,
                    NSCompositingOperation::SourceOver,
                    1.0,
                    true,
                    None,
                )
            };
            image.drawAtPoint_fromRect_operation_fraction(
                pt(12.0, 0.0),
                NSRect::ZERO,
                NSCompositingOperation::SourceOver,
                1.0,
            );
        });
        let rep = snapshot(&view, 1.0);
        // drawInRect: and respectFlipped: are upright either way; the
        // older methods are upside down in a flipped view.
        let (upright, older) = (RED, if flipped { BLUE } else { RED });
        assert_px(&rep, 1, 0, upright);
        assert_px(&rep, 5, 0, older);
        assert_px(&rep, 9, 0, upright);
        assert_px(&rep, 13, 0, older);
        assert_px(&rep, 13, 3, if flipped { RED } else { BLUE });
    }
    // Part of an image: its top half (image coordinates start at the
    // bottom) fills the rectangle.
    let rep = bitmap(4, 4);
    draw_in(&rep, |_| {
        halves.drawInRect_fromRect_operation_fraction(
            rect(0.0, 0.0, 4.0, 4.0),
            rect(0.0, 2.0, 4.0, 2.0),
            NSCompositingOperation::SourceOver,
            1.0,
        )
    });
    assert_px(&rep, 1, 0, RED);
    assert_px(&rep, 1, 1, RED);
}

fn flipped_images(_: MainThreadMarker) {
    // A flipped image draws upside down, by either method, in an unflipped
    // context.
    let image = from_file("halves.png");
    image.setFlipped(true);
    let rep = bitmap(8, 4);
    draw_in(&rep, |_| {
        image.drawInRect(rect(0.0, 0.0, 4.0, 4.0));
        image.drawInRect_fromRect_operation_fraction(
            rect(4.0, 0.0, 4.0, 4.0),
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            1.0,
        );
    });
    assert_px(&rep, 1, 0, BLUE);
    assert_px(&rep, 1, 3, RED);
    assert_px(&rep, 5, 0, BLUE);
    assert_px(&rep, 5, 3, RED);
}

fn copies(_: MainThreadMarker) {
    // A bitmap's copy has its own pixels, the same to begin with.
    let rep = bitmap(2, 2);
    fill_bitmap(&rep, RED);
    rep.setSize(NSSize::new(1.0, 1.0));
    let copy = rep.copy();
    assert!(!std::ptr::eq(&*rep, &*copy) && rep.bitmapData() != copy.bitmapData());
    assert_eq!((copy.pixelsWide(), copy.pixelsHigh(), copy.size()), (2, 2, NSSize::new(1.0, 1.0)));
    assert_eq!(bytes_at(&copy, 1, 1), RED);
    fill_bitmap(&copy, BLUE);
    assert_eq!(bytes_at(&rep, 1, 1), RED, "the original keeps its pixels");
    // A drawing handler's copy draws the same.
    let handler = RcBlock::new(|r: NSRect| {
        NSColor::greenColor().setFill();
        NSRectFill(r);
        Bool::YES
    });
    let custom = NSCustomImageRep::initWithSize_flipped_drawingHandler(
        NSCustomImageRep::alloc(),
        NSSize::new(2.0, 2.0),
        false,
        &handler,
    );
    let other = custom.copy();
    assert_eq!(other.size(), NSSize::new(2.0, 2.0));
    let target = bitmap(2, 2);
    draw_in(&target, |_| other.draw());
    assert_px(&target, 1, 1, GREEN);
    // An image's copy has representations of its own.
    let image = NSImage::new();
    image.addRepresentation(&rep);
    // SAFETY: -copy returns a new image.
    let dup: Retained<NSImage> = unsafe { msg_send![&*image, copy] };
    let theirs = only_bitmap(&dup);
    assert!(!std::ptr::eq(&*theirs, &*rep), "a new representation");
    fill_bitmap(&theirs, GREEN);
    theirs.setSize(NSSize::new(9.0, 9.0));
    assert_eq!(bytes_at(&rep, 0, 0), RED);
    assert_eq!(rep.size(), NSSize::new(1.0, 1.0));
}

fn compositing(_: MainThreadMarker) {
    let halves = from_file("halves.png");
    // A fraction fades the image.
    let rep = bitmap(4, 4);
    fill_bitmap(&rep, WHITE);
    draw_in(&rep, |_| {
        halves.drawInRect_fromRect_operation_fraction(
            rect(0.0, 0.0, 4.0, 4.0),
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            0.5,
        )
    });
    assert_px(&rep, 0, 0, [255, 127, 127, 255]);
    assert_px(&rep, 0, 3, [127, 127, 255, 255]);
    // Copy replaces what's there, alpha included.
    let rep = bitmap(4, 4);
    fill_bitmap(&rep, WHITE);
    draw_in(&rep, |_| {
        halves.drawInRect_fromRect_operation_fraction(
            rect(0.0, 0.0, 4.0, 4.0),
            NSRect::ZERO,
            NSCompositingOperation::Copy,
            0.5,
        )
    });
    assert_px(&rep, 0, 0, [128, 0, 0, 128]);
    let rgba = from_file("rgba-72dpi.png");
    let rep = bitmap(4, 2);
    fill_bitmap(&rep, BLUE);
    draw_in(&rep, |_| {
        rgba.drawInRect_fromRect_operation_fraction(
            rect(0.0, 0.0, 4.0, 2.0),
            NSRect::ZERO,
            NSCompositingOperation::Copy,
            1.0,
        )
    });
    assert_px(&rep, 0, 1, [128, 0, 0, 128]);
    assert_px(&rep, 1, 1, [64, 64, 64, 255]);
    // An image with nothing in it draws nothing.
    let rep = bitmap(2, 2);
    draw_in(&rep, |_| {
        NSImage::initWithSize(NSImage::alloc(), NSSize::new(2.0, 2.0)).drawInRect(rect(0.0, 0.0, 2.0, 2.0))
    });
    assert_px(&rep, 0, 0, CLEAR);
    // A template image draws its own colors.
    let template = from_file("halves.png");
    assert!(!template.isTemplate());
    template.setTemplate(true);
    let rep = bitmap(4, 4);
    draw_in(&rep, |_| template.drawInRect(rect(0.0, 0.0, 4.0, 4.0)));
    assert_px(&rep, 0, 0, RED);
    assert_px(&rep, 0, 3, BLUE);
}

fn representation_choice(_: MainThreadMarker) {
    // A red 1× bitmap and a blue 2× one.
    let one = bitmap(2, 2);
    fill_bitmap(&one, RED);
    let two = bitmap(4, 4);
    fill_bitmap(&two, BLUE);
    two.setSize(NSSize::new(2.0, 2.0));
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(2.0, 2.0));
    image.addRepresentation(&one);
    image.addRepresentation(&two);
    assert_eq!(image.representations().count(), 2);
    let draw = |scale: f64, points: f64| {
        let n = (points * scale) as isize;
        let rep = bitmap(n, n);
        rep.setSize(NSSize::new(points, points));
        draw_in(&rep, |_| image.drawInRect(rect(0.0, 0.0, points, points)));
        pixel(&rep, 0, 0)
    };
    assert_eq!(draw(1.0, 2.0), RED, "1× at 1×");
    assert_eq!(draw(2.0, 2.0), BLUE, "2× at 2×");
    assert_eq!(draw(1.0, 4.0), BLUE, "twice as large at 1× needs the 2× pixels");
    image.removeRepresentation(&two);
    assert_eq!(draw(2.0, 2.0), RED, "the one left");

    // Pixels written after a fresh bitmapData show when drawn again.
    let pixels = bitmap(2, 2);
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(2.0, 2.0));
    image.addRepresentation(&pixels);
    let rep = bitmap(2, 2);
    draw_in(&rep, |_| image.drawInRect(rect(0.0, 0.0, 2.0, 2.0)));
    fill_bitmap(&pixels, GREEN);
    draw_in(&rep, |_| image.drawInRect(rect(0.0, 0.0, 2.0, 2.0)));
    assert_px(&rep, 0, 0, GREEN);
}

fn drawing_handlers(_: MainThreadMarker) {
    let calls: Rc<RefCell<Vec<(NSRect, bool)>>> = Rc::default();
    let seen = calls.clone();
    let handler = RcBlock::new(move |r: NSRect| -> Bool {
        let flipped = NSGraphicsContext::currentContext().expect("a context").isFlipped();
        seen.borrow_mut().push((r, flipped));
        NSColor::redColor().setFill();
        NSRectFill(rect(0.0, 0.0, 2.0, 1.0));
        Bool::YES
    });
    let appearance = |name| NSAppearance::appearanceNamed(name).expect("an appearance");
    // SAFETY: constant strings.
    let (aqua, dark) = unsafe { (appearance(NSAppearanceNameAqua), appearance(NSAppearanceNameDarkAqua)) };
    let within = |a: &NSAppearance, f: &dyn Fn()| a.performAsCurrentDrawingAppearance(&RcBlock::new(f));
    for flipped in [false, true] {
        calls.borrow_mut().clear();
        let image = NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(4.0, 2.0), flipped, &handler);
        assert!(calls.borrow().is_empty(), "not called when made");
        assert_eq!(image.size(), NSSize::new(4.0, 2.0));
        let reps = image.representations();
        assert!(reps.count() == 1 && reps.objectAtIndex(0).downcast::<NSCustomImageRep>().is_ok());
        let rep = bitmap(4, 2);
        within(&aqua, &|| draw_in(&rep, |_| image.drawInRect(rect(0.0, 0.0, 4.0, 2.0))));
        assert_eq!(*calls.borrow(), [(rect(0.0, 0.0, 4.0, 2.0), flipped)], "called with its bounds, flipped as asked");
        // The handler's (0, 0) is at the top when flipped.
        let (red_row, clear_row) = if flipped { (0, 1) } else { (1, 0) };
        assert_px(&rep, 0, red_row, RED);
        assert_px(&rep, 3, red_row, CLEAR);
        assert_px(&rep, 0, clear_row, CLEAR);
        // Kept: drawn again the same, it isn't called; at another scale
        // or in another appearance it is.
        within(&aqua, &|| draw_in(&rep, |_| image.drawInRect(rect(0.0, 0.0, 4.0, 2.0))));
        assert_eq!(calls.borrow().len(), 1);
        let twice = bitmap(8, 4);
        twice.setSize(NSSize::new(4.0, 2.0));
        within(&aqua, &|| draw_in(&twice, |_| image.drawInRect(rect(0.0, 0.0, 4.0, 2.0))));
        assert_eq!(calls.borrow().len(), 2, "at 2×");
        within(&dark, &|| draw_in(&rep, |_| image.drawInRect(rect(0.0, 0.0, 4.0, 2.0))));
        assert_eq!(calls.borrow().len(), 3, "in another appearance");
    }
}

fn bitmap_layout(_: MainThreadMarker) {
    // SAFETY: NULL planes make the rep allocate its own.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            3,
            2,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }
    .expect("a rep");
    // Rows rounded up to 32 bytes; premultiplied, alpha last; a point a
    // pixel.
    assert_eq!((rep.bytesPerRow(), rep.bitsPerPixel(), rep.numberOfPlanes()), (32, 32, 1));
    assert_eq!(rep.bitmapFormat(), NSBitmapFormat::empty());
    assert_eq!(rep.size(), NSSize::new(3.0, 2.0));
    assert_eq!((rep.pixelsWide(), rep.pixelsHigh()), (3, 2));
    // setColor: stores premultiplied samples; colorAtX:y: reads them back
    // (top-left origin).
    rep.setColor_atX_y(&NSColor::colorWithDeviceRed_green_blue_alpha(1.0, 0.5, 0.25, 0.5), 2, 1);
    assert!(close(bytes_at(&rep, 2, 1), [127, 63, 31, 127], 1), "{:?}", bytes_at(&rep, 2, 1));
    let c = rep.colorAtX_y(2, 1).expect("a color");
    let got = [c.redComponent(), c.greenComponent(), c.blueComponent(), c.alphaComponent()];
    assert!(got.iter().zip([1.0, 0.5, 0.25, 0.5]).all(|(a, b)| (a - b).abs() < 0.01), "{got:?}");
    let mut samples = [0usize; 4];
    // SAFETY: room for four samples.
    unsafe { rep.getPixel_atX_y(NonNull::new(samples.as_mut_ptr()).unwrap(), 2, 1) };
    assert!(samples.iter().zip([127, 63, 31, 127]).all(|(a, b)| a.abs_diff(b) <= 1), "{samples:?}");
    let mut set = [10usize, 20, 30, 40];
    // SAFETY: four samples.
    unsafe { rep.setPixel_atX_y(NonNull::new(set.as_mut_ptr()).unwrap(), 0, 0) };
    assert_eq!(bytes_at(&rep, 0, 0), [10, 20, 30, 40]);
    // Row 0 of bitmapData is the top: a fill at the bottom left lands in
    // the last row.
    let target = bitmap(3, 2);
    draw_in(&target, |_| {
        NSColor::blueColor().setFill();
        NSRectFill(rect(0.0, 0.0, 1.0, 1.0));
    });
    assert_eq!(bytes_at(&target, 0, 1), BLUE);
    assert_eq!(bytes_at(&target, 0, 0), CLEAR);
}

fn encoding(_: MainThreadMarker) {
    if !has_data() {
        return;
    }
    let src = only_bitmap(&from_file("halves.png"));
    // SAFETY: no properties.
    let png = unsafe { src.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
        .expect("a PNG");
    assert_eq!(&png.to_vec()[..8], b"\x89PNG\r\n\x1a\n");
    let back = NSBitmapImageRep::imageRepWithData(&png).expect("decodes");
    for (x, y) in [(0, 0), (3, 1), (0, 2), (3, 3)] {
        assert_eq!(bytes_at(&back, x, y), bytes_at(&src, x, y), "({x}, {y})");
    }
    // SAFETY: a constant key.
    let key = unsafe { NSImageCompressionFactor };
    let factor = NSNumber::new_f64(0.3);
    let props = NSDictionary::from_slices(&[key], &[&*factor as &AnyObject]);
    // SAFETY: the dictionary holds a compression factor.
    let jpeg = unsafe { src.representationUsingType_properties(NSBitmapImageFileType::JPEG, &props) }.expect("a JPEG");
    assert_eq!(&jpeg.to_vec()[..2], [0xff, 0xd8]);
    // An image's TIFF reads back as the same image.
    let tiff = from_file("halves.png").TIFFRepresentation().expect("a TIFF");
    let image = NSImage::initWithData(NSImage::alloc(), &tiff).expect("decodes");
    assert_eq!(image.size(), NSSize::new(4.0, 4.0));
    assert!(close(bytes_at(&only_bitmap(&image), 0, 0), RED, 1));
    assert!(
        NSImage::initWithSize(NSImage::alloc(), NSSize::new(3.0, 3.0)).TIFFRepresentation().is_none(),
        "nothing to encode"
    );
}

fn lock_focus(_: MainThreadMarker) {
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(3.0, 2.0));
    image.lockFocus();
    let ctx = NSGraphicsContext::currentContext().expect("a context while focused");
    assert!(!ctx.isFlipped());
    NSColor::redColor().setFill();
    NSRectFill(rect(0.0, 0.0, 1.0, 1.0));
    image.unlockFocus();
    assert!(NSGraphicsContext::currentContext().is_none(), "put back");
    assert_eq!((image.size(), image.representations().count()), (NSSize::new(3.0, 2.0), 1));
    let rep = bitmap(3, 2);
    draw_in(&rep, |_| image.drawInRect(rect(0.0, 0.0, 3.0, 2.0)));
    assert!(pixel(&rep, 0, 1)[3] > 128 && pixel(&rep, 0, 0)[3] < 128, "drawn at the bottom left");
    let flipped = NSImage::initWithSize(NSImage::alloc(), NSSize::new(3.0, 2.0));
    flipped.lockFocusFlipped(true);
    assert!(NSGraphicsContext::currentContext().expect("a context").isFlipped());
    flipped.unlockFocus();
    let _ = Cell::new(0);
}

fn symbol_images(_: MainThreadMarker) {
    let symbol = |name: &str| {
        NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(name),
            Some(&NSString::from_str("a label")),
        )
    };
    assert!(symbol("no.such.symbol.anywhere").is_none());
    for name in ["star", "star.fill", "plus", "xmark", "chevron.right", "magnifyingglass", "gearshape", "trash"] {
        let image = symbol(name).unwrap_or_else(|| panic!("{name}"));
        assert!(image.isTemplate(), "{name} is a template");
        assert_eq!(image.accessibilityDescription().map(|d| d.to_string()).as_deref(), Some("a label"));
        assert!(image.name().is_none());
        assert_eq!(image.representations().count(), 1);
        let size = image.size();
        assert!(
            size.width >= 4.0 && size.width <= 30.0 && size.height >= 8.0 && size.height <= 30.0,
            "{name}: {size:?}"
        );
        // A larger point size or scale makes a larger image.
        let big = image
            .imageWithSymbolConfiguration(&NSImageSymbolConfiguration::configurationWithPointSize_weight(26.0, 0.0))
            .expect("an image");
        assert!(
            big.size().width > size.width * 1.5 && big.size().height > size.height * 1.5,
            "{name}: {:?}",
            big.size()
        );
        let large = image
            .imageWithSymbolConfiguration(&NSImageSymbolConfiguration::configurationWithScale(
                NSImageSymbolScale::Large,
            ))
            .expect("an image");
        assert!(large.size().width > size.width && large.size().height > size.height, "{name}");
        assert!(large.isTemplate());
    }
    // Drawn as they are, black; views tint templates.
    let star = symbol("star.fill").expect("a star");
    let rep = bitmap(20, 20);
    draw_in(&rep, |_| star.drawInRect(rect(0.0, 0.0, 20.0, 20.0)));
    assert_px(&rep, 10, 10, BLACK);
    assert_px(&rep, 0, 0, CLEAR);
    // Other images don't change with a configuration.
    let plain = NSImage::initWithSize(NSImage::alloc(), NSSize::new(4.0, 4.0));
    let same = plain
        .imageWithSymbolConfiguration(&NSImageSymbolConfiguration::configurationWithScale(NSImageSymbolScale::Large));
    assert!(same.is_some_and(|s| std::ptr::eq(&*s, &*plain)));
    let _ = plain.symbolConfiguration();
    // Symbols of the program's own come from its asset catalog.
    assert!(NSImage::imageWithSymbolName_variableValue(&NSString::from_str("star"), 0.5).is_none());
    let merged = NSImageSymbolConfiguration::configurationWithScale(NSImageSymbolScale::Small)
        .configurationByApplyingConfiguration(&NSImageSymbolConfiguration::configurationWithPointSize_weight(
            20.0, 0.3,
        ));
    let _ = merged;
}

fn exported_names(_: MainThreadMarker) {
    let s = |v: &NSString| v.to_string();
    // SAFETY: constant strings.
    unsafe {
        assert_eq!(s(NSImageHintCTM), "NSImageHintCTM");
        assert_eq!(s(NSImageHintInterpolation), "NSImageHintInterpolation");
        assert_eq!(s(NSImageHintUserInterfaceLayoutDirection), "NSImageHintUserInterfaceLayoutDirection");
        for (key, value) in [
            (NSImageCompressionMethod, "NSImageCompressionMethod"),
            (NSImageCompressionFactor, "NSImageCompressionFactor"),
            (NSImageDitherTransparency, "NSImageDitherTransparency"),
            (NSImageRGBColorTable, "NSImageRGBColorTable"),
            (NSImageInterlaced, "NSImageInterlaced"),
            (NSImageColorSyncProfileData, "NSImageColorSyncProfileData"),
            (NSImageFrameCount, "NSImageFrameCount"),
            (NSImageCurrentFrame, "NSImageCurrentFrame"),
            (NSImageCurrentFrameDuration, "NSImageCurrentFrameDuration"),
            (NSImageLoopCount, "NSImageLoopCount"),
            (NSImageGamma, "NSImageGamma"),
            (NSImageProgressive, "NSImageProgressive"),
            (NSImageEXIFData, "NSImageEXIFData"),
            (NSImageIPTCData, "NSImageIPTCData"),
            (NSImageFallbackBackgroundColor, "NSImageFallbackBackgroundColor"),
        ] {
            assert_eq!(s(key), value);
        }
    }
    assert_eq!(NSImageRep::imageTypes().count(), 0, "the abstract class reads nothing");
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("basics", basics),
        ("decoding", decoding),
        ("draw_orientation", draw_orientation),
        ("flipped_images", flipped_images),
        ("copies", copies),
        ("compositing", compositing),
        ("representation_choice", representation_choice),
        ("drawing_handlers", drawing_handlers),
        ("bitmap_layout", bitmap_layout),
        ("encoding", encoding),
        ("lock_focus", lock_focus),
        ("symbol_images", symbol_images),
        ("exported_names", exported_names),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
