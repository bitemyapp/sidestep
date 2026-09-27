//! A gallery of drawing: shapes, colors, images and view opacity, each a
//! window of labeled tiles. Written only against objc2-app-kit: on macOS
//! it runs on AppKit, on Linux on Sidestep, so the two can be compared
//! side by side (and in screenshots, under `scripts/headless-wayland`).
//!
//! SCENARIO: `shapes` (the default): fills, strokes, curves, clips,
//! transforms, gradients and shadows; `colors`: the system colors in the
//! window's appearance; `images`: bitmaps, drawing handlers, scaling and
//! compositing; `symbols`: symbol images, tinted; `alpha`: views with
//! `alphaValue`, set through `animator`;
//! `bench`: a frame of 2,000 rounded-rect fills, 1,000 strokes and 300
//! image draws (half of them downscaled), timed; `cg`: CoreGraphics, the
//! same kinds of drawing through `-[NSGraphicsContext CGContext]`;
//! `cgbench`: the bench frame through CoreGraphics calls; `ct`: CoreText,
//! lines, glyphs and a frame in DejaVu Sans loaded from a file.
//! GALLERY_QUIT_AFTER: seconds until the app terminates itself.
//! GALLERY_APPEARANCE: `light` or `dark` sets the application's
//! appearance (else it follows the desktop's).
//! GALLERY_PNG=path: open no window; draw the scenario into a bitmap with
//! `cacheDisplayInRect:toBitmapImageRep:` (at GALLERY_SCALE pixels a
//! point, 1 by default) and write it as a PNG file, or for `bench`, print
//! the timings of drawing into bitmaps. Nothing is shown, so this runs
//! anywhere, a Mac included, without taking the screen. (Partial
//! `alphaValue` isn't drawn this way, on either system.)

use std::cell::{Cell, OnceCell, RefCell};
use std::ptr::NonNull;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAffineTransformNSAppKitAdditions, NSAnimatablePropertyContainer, NSAnimationContext, NSAppearance,
    NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSApplicationActivationPolicy,
    NSApplicationDelegate, NSBackingStoreType, NSBezierPath, NSBitmapFormat, NSBitmapImageRep, NSColor,
    NSCompositingOperation, NSDeviceRGBColorSpace, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSGradient, NSGradientDrawingOptions, NSGraphicsContext, NSImage, NSLineCapStyle, NSLineJoinStyle, NSRectFill,
    NSRectFillUsingOperation, NSResponder, NSShadow, NSStringDrawing, NSView, NSWindingRule, NSWindow,
    NSWindowStyleMask,
};
use objc2_core_foundation::{CFRetained, CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGBlendMode, CGColor, CGColorSpace, CGContext, CGGradient,
    CGGradientDrawingOptions, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGLineCap, CGLineJoin, CGMutablePath,
    CGPath, CGPathDrawingMode, CGTextDrawingMode, kCGColorSpaceSRGB,
};
use objc2_foundation::{
    NSAffineTransform, NSArray, NSDictionary, NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString, NSTimer, ns_string,
};

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

const TILE: f64 = 150.0;
const GAP: f64 = 12.0;
const COLUMNS: usize = 6;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn srgb(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a)
}

fn label(text: &str, at: NSPoint) {
    let font = NSFont::systemFontOfSize(11.0);
    let color = NSColor::labelColor();
    // SAFETY: constant keys.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [&font, &color];
    let attrs = NSDictionary::from_slices(&keys, &values);
    // SAFETY: the attributes hold a font and a color.
    unsafe { NSString::from_str(text).drawAtPoint_withAttributes(at, Some(&attrs)) };
}

/// Tile `i`'s drawing area (flipped view coordinates), below its label.
fn tile(i: usize) -> NSRect {
    let (col, row) = (i % COLUMNS, i / COLUMNS);
    rect(GAP + col as f64 * (TILE + GAP), GAP + row as f64 * (TILE + 18.0 + GAP) + 18.0, TILE, TILE)
}

/// Draw tile `i`, titled: `f` gets its rectangle, with the graphics state
/// saved around it.
fn with_tile(i: usize, title: &str, f: impl FnOnce(NSRect)) {
    let r = tile(i);
    label(title, NSPoint::new(r.origin.x, r.origin.y - 16.0));
    NSColor::quaternaryLabelColor().setFill();
    NSBezierPath::fillRect(r);
    NSGraphicsContext::saveGraphicsState_class();
    f(r);
    NSGraphicsContext::restoreGraphicsState_class();
}

fn inset(r: NSRect, d: f64) -> NSRect {
    rect(r.origin.x + d, r.origin.y + d, r.size.width - 2.0 * d, r.size.height - 2.0 * d)
}

fn center(r: NSRect) -> NSPoint {
    NSPoint::new(r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0)
}

fn at(r: NSRect, x: f64, y: f64) -> NSPoint {
    NSPoint::new(r.origin.x + x, r.origin.y + y)
}

fn shapes() {
    let red = srgb(0.88, 0.11, 0.14, 1.0);
    let blue = srgb(0.21, 0.52, 0.89, 1.0);
    let green = srgb(0.18, 0.76, 0.49, 1.0);
    with_tile(0, "fills", |r| {
        red.setFill();
        NSRectFill(rect(r.origin.x + 10.0, r.origin.y + 10.0, 60.0, 60.0));
        srgb(0.21, 0.52, 0.89, 0.6).setFill();
        NSBezierPath::fillRect(rect(r.origin.x + 40.0, r.origin.y + 40.0, 70.0, 70.0));
        green.setFill();
        NSBezierPath::bezierPathWithOvalInRect(rect(r.origin.x + 80.0, r.origin.y + 80.0, 60.0, 60.0)).fill();
    });
    with_tile(1, "rounded rects", |r| {
        for (k, radius) in [2.0, 8.0, 15.0, 40.0].into_iter().enumerate() {
            let y = r.origin.y + 8.0 + k as f64 * 35.0;
            NSColor::systemBlueColor().setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                rect(r.origin.x + 8.0, y, 134.0, 30.0),
                radius,
                radius,
            )
            .fill();
        }
    });
    with_tile(2, "caps and joins", |r| {
        NSColor::labelColor().setStroke();
        let styles = [
            (NSLineCapStyle::Butt, NSLineJoinStyle::Miter),
            (NSLineCapStyle::Round, NSLineJoinStyle::Round),
            (NSLineCapStyle::Square, NSLineJoinStyle::Bevel),
        ];
        for (k, (cap, join)) in styles.into_iter().enumerate() {
            let x = 20.0 + k as f64 * 45.0;
            let p = NSBezierPath::bezierPath();
            p.moveToPoint(at(r, x, 20.0));
            p.lineToPoint(at(r, x + 25.0, 70.0));
            p.lineToPoint(at(r, x, 120.0));
            p.setLineWidth(9.0);
            p.setLineCapStyle(cap);
            p.setLineJoinStyle(join);
            p.stroke();
        }
    });
    with_tile(3, "hairlines and dashes", |r| {
        NSColor::labelColor().setStroke();
        for (k, width) in [0.0, 0.5, 1.0, 2.0, 4.0].into_iter().enumerate() {
            let y = 12.5 + k as f64 * 14.0;
            let p = NSBezierPath::bezierPath();
            p.moveToPoint(at(r, 8.0, y));
            p.lineToPoint(at(r, 142.0, y));
            p.setLineWidth(width);
            p.stroke();
        }
        NSColor::systemOrangeColor().setStroke();
        for (k, dash) in [[4.0, 4.0], [12.0, 3.0], [1.0, 3.0]].into_iter().enumerate() {
            let y = 90.0 + k as f64 * 18.0;
            let p = NSBezierPath::bezierPath();
            p.moveToPoint(at(r, 8.0, y));
            p.lineToPoint(at(r, 142.0, y));
            p.setLineWidth(3.0);
            // SAFETY: two lengths.
            unsafe { p.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
            p.stroke();
        }
    });
    with_tile(4, "curves and arcs", |r| {
        let p = NSBezierPath::bezierPath();
        p.moveToPoint(at(r, 10.0, 140.0));
        p.curveToPoint_controlPoint1_controlPoint2(at(r, 140.0, 140.0), at(r, 40.0, 10.0), at(r, 110.0, 10.0));
        p.setLineWidth(3.0);
        NSColor::systemPurpleColor().setStroke();
        p.stroke();
        let arc = NSBezierPath::bezierPath();
        let c = center(r);
        arc.moveToPoint(c);
        arc.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle(c, 40.0, 20.0, 290.0);
        arc.closePath();
        NSColor::systemTealColor().setFill();
        arc.fill();
    });
    with_tile(5, "non-zero and even-odd", |r| {
        for (k, rule) in [NSWindingRule::NonZero, NSWindingRule::EvenOdd].into_iter().enumerate() {
            let x = r.origin.x + 8.0 + k as f64 * 70.0;
            let p = NSBezierPath::bezierPathWithOvalInRect(rect(x, r.origin.y + 40.0, 64.0, 64.0));
            p.appendBezierPathWithOvalInRect(rect(x + 16.0, r.origin.y + 56.0, 32.0, 32.0));
            p.setWindingRule(rule);
            green.setFill();
            p.fill();
        }
    });
    with_tile(6, "transforms", |r| {
        let c = center(r);
        for k in 0..12 {
            NSGraphicsContext::saveGraphicsState_class();
            let t = NSAffineTransform::transform();
            t.translateXBy_yBy(c.x, c.y);
            t.rotateByDegrees(k as f64 * 30.0);
            t.concat();
            srgb(k as f64 / 12.0, 0.4, 1.0 - k as f64 / 12.0, 1.0).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect(20.0, -6.0, 45.0, 12.0), 6.0, 6.0).fill();
            NSGraphicsContext::restoreGraphicsState_class();
        }
    });
    with_tile(7, "clips", |r| {
        NSBezierPath::bezierPathWithOvalInRect(inset(r, 10.0)).addClip();
        for k in 0..15 {
            if k % 2 == 0 {
                red.setFill()
            } else {
                blue.setFill()
            }
            NSRectFill(rect(r.origin.x + k as f64 * 10.0, r.origin.y, 10.0, r.size.height));
        }
    });
    with_tile(8, "linear gradients", |r| {
        let g = NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &red, &blue).expect("a gradient");
        g.drawInRect_angle(rect(r.origin.x + 8.0, r.origin.y + 8.0, 134.0, 40.0), 0.0);
        g.drawInBezierPath_angle(
            &NSBezierPath::bezierPathWithOvalInRect(rect(r.origin.x + 8.0, r.origin.y + 56.0, 60.0, 86.0)),
            90.0,
        );
        let three = NSArray::from_retained_slice(&[red.clone(), NSColor::systemYellowColor(), green.clone()]);
        let g = NSGradient::initWithColors(NSGradient::alloc(), &three).expect("a gradient");
        let rounded = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            rect(r.origin.x + 76.0, r.origin.y + 56.0, 66.0, 86.0),
            12.0,
            12.0,
        );
        g.drawInBezierPath_angle(&rounded, 45.0);
    });
    with_tile(9, "radial gradients", |r| {
        let g = NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &NSColor::whiteColor(), &blue)
            .expect("a gradient");
        g.drawInRect_relativeCenterPosition(
            rect(r.origin.x + 8.0, r.origin.y + 8.0, 64.0, 64.0),
            NSPoint::new(-0.4, 0.4),
        );
        let c = at(r, 110.0, 40.0);
        g.drawFromCenter_radius_toCenter_radius_options(c, 5.0, c, 30.0, NSGradientDrawingOptions::empty());
        let g = NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &NSColor::systemYellowColor(), &red)
            .expect("a gradient");
        g.drawFromCenter_radius_toCenter_radius_options(
            at(r, 60.0, 110.0),
            0.0,
            at(r, 75.0, 110.0),
            35.0,
            NSGradientDrawingOptions::empty(),
        );
    });
    with_tile(10, "shadows", |r| {
        let s = NSShadow::new();
        s.setShadowOffset(NSSize::new(4.0, -4.0));
        s.setShadowBlurRadius(6.0);
        s.set();
        NSColor::controlBackgroundColor().setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            rect(r.origin.x + 16.0, r.origin.y + 16.0, 118.0, 50.0),
            10.0,
            10.0,
        )
        .fill();
        let s = NSShadow::new();
        s.setShadowBlurRadius(10.0);
        s.setShadowColor(Some(&NSColor::systemRedColor()));
        s.set();
        red.setFill();
        NSBezierPath::bezierPathWithOvalInRect(rect(r.origin.x + 50.0, r.origin.y + 84.0, 50.0, 50.0)).fill();
    });
    with_tile(11, "compositing", |r| {
        let ops = [
            NSCompositingOperation::SourceOver,
            NSCompositingOperation::Multiply,
            NSCompositingOperation::Screen,
            NSCompositingOperation::Difference,
        ];
        let context = NSGraphicsContext::currentContext().expect("a context");
        for (k, op) in ops.into_iter().enumerate() {
            let (x, y) = (r.origin.x + 8.0 + (k % 2) as f64 * 70.0, r.origin.y + 8.0 + (k / 2) as f64 * 70.0);
            blue.setFill();
            NSRectFill(rect(x, y, 40.0, 40.0));
            context.setCompositingOperation(op);
            srgb(0.95, 0.6, 0.1, 1.0).setFill();
            NSBezierPath::bezierPathWithOvalInRect(rect(x + 20.0, y + 20.0, 40.0, 40.0)).fill();
            context.setCompositingOperation(NSCompositingOperation::SourceOver);
        }
    });
}

fn colors() {
    type Make = fn() -> Retained<NSColor>;
    let named: &[(&str, Make)] = &[
        ("label", NSColor::labelColor),
        ("secondaryLabel", NSColor::secondaryLabelColor),
        ("tertiaryLabel", NSColor::tertiaryLabelColor),
        ("quaternaryLabel", NSColor::quaternaryLabelColor),
        ("text", NSColor::textColor),
        ("link", NSColor::linkColor),
        ("placeholderText", NSColor::placeholderTextColor),
        ("separator", NSColor::separatorColor),
        ("grid", NSColor::gridColor),
        ("windowBackground", NSColor::windowBackgroundColor),
        ("underPageBackground", NSColor::underPageBackgroundColor),
        ("controlBackground", NSColor::controlBackgroundColor),
        ("textBackground", NSColor::textBackgroundColor),
        ("control", NSColor::controlColor),
        ("controlAccent", NSColor::controlAccentColor),
        ("selectedContentBackground", NSColor::selectedContentBackgroundColor),
        ("unemphasizedSelection", NSColor::unemphasizedSelectedContentBackgroundColor),
        ("selectedTextBackground", NSColor::selectedTextBackgroundColor),
        ("keyboardFocusIndicator", NSColor::keyboardFocusIndicatorColor),
        ("findHighlight", NSColor::findHighlightColor),
        ("systemFill", NSColor::systemFillColor),
        ("secondarySystemFill", NSColor::secondarySystemFillColor),
        ("systemRed", NSColor::systemRedColor),
        ("systemOrange", NSColor::systemOrangeColor),
        ("systemYellow", NSColor::systemYellowColor),
        ("systemGreen", NSColor::systemGreenColor),
        ("systemMint", NSColor::systemMintColor),
        ("systemTeal", NSColor::systemTealColor),
        ("systemCyan", NSColor::systemCyanColor),
        ("systemBlue", NSColor::systemBlueColor),
        ("systemIndigo", NSColor::systemIndigoColor),
        ("systemPurple", NSColor::systemPurpleColor),
        ("systemPink", NSColor::systemPinkColor),
        ("systemBrown", NSColor::systemBrownColor),
        ("systemGray", NSColor::systemGrayColor),
        ("shadow", NSColor::shadowColor),
    ];
    for (i, (name, make)) in named.iter().enumerate() {
        let (col, row) = (i % 4, i / 4);
        let r = rect(GAP + col as f64 * 245.0, GAP + row as f64 * 38.0, 36.0, 30.0);
        NSColor::separatorColor().setFill();
        NSBezierPath::fillRect(r);
        make().setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(inset(r, 1.0), 5.0, 5.0).fill();
        label(name, NSPoint::new(r.origin.x + 44.0, r.origin.y + 8.0));
    }
}

/// A `w` × `h` bitmap of 8-bit RGBA, filled by `f` (x and y from the top
/// left) through `bitmapData`.
fn bitmap(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 4]) -> Retained<NSBitmapImageRep> {
    // SAFETY: NULL planes make the rep allocate its own.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w as isize,
            h as isize,
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
    .expect("a bitmap");
    let (data, row) = (rep.bitmapData(), rep.bytesPerRow() as usize);
    for y in 0..h {
        for x in 0..w {
            // SAFETY: inside the bitmap's rows.
            unsafe { std::ptr::write(data.add(y * row + x * 4).cast::<[u8; 4]>(), f(x, y)) };
        }
    }
    rep
}

/// A colorful checkerboard, `n` pixels (and points) square.
fn card(n: usize) -> Retained<NSImage> {
    let cell = (n / 8).max(1);
    let rep = bitmap(n, n, |x, y| {
        if (x / cell + y / cell).is_multiple_of(2) {
            [(x * 255 / n) as u8, (y * 255 / n) as u8, 200, 255]
        } else {
            [240, 240, 240, 255]
        }
    });
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(n as f64, n as f64));
    image.addRepresentation(&rep);
    image
}

/// A star drawn by a drawing handler, in the label color.
fn star() -> Retained<NSImage> {
    let handler = RcBlock::new(|r: NSRect| -> Bool {
        let c = center(r);
        let p = NSBezierPath::bezierPath();
        for k in 0..10 {
            let a = std::f64::consts::FRAC_PI_2 + k as f64 * std::f64::consts::PI / 5.0;
            let radius = if k % 2 == 0 { r.size.width / 2.0 } else { r.size.width / 5.0 };
            let q = NSPoint::new(c.x + radius * a.cos(), c.y + radius * a.sin());
            if k == 0 { p.moveToPoint(q) } else { p.lineToPoint(q) }
        }
        p.closePath();
        NSColor::labelColor().setFill();
        p.fill();
        Bool::YES
    });
    NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(48.0, 48.0), false, &handler)
}

fn images() {
    let card64 = card(64);
    let big = card(1024);
    let star = star();
    with_tile(0, "bitmap, 1:1 and stretched", |r| {
        card64.drawInRect(rect(r.origin.x + 8.0, r.origin.y + 8.0, 64.0, 64.0));
        card64.drawInRect(rect(r.origin.x + 8.0, r.origin.y + 78.0, 134.0, 64.0));
    });
    with_tile(1, "1024 px drawn small", |r| big.drawInRect(inset(r, 8.0)));
    with_tile(2, "drawing handler", |r| {
        star.drawInRect(rect(r.origin.x + 8.0, r.origin.y + 8.0, 48.0, 48.0));
        star.drawInRect(rect(r.origin.x + 56.0, r.origin.y + 52.0, 90.0, 90.0));
    });
    with_tile(3, "fractions", |r| {
        for (k, fraction) in [1.0, 0.6, 0.3].into_iter().enumerate() {
            let d = 8.0 + k as f64 * 38.0;
            card64.drawInRect_fromRect_operation_fraction(
                rect(r.origin.x + d, r.origin.y + d, 64.0, 64.0),
                NSRect::ZERO,
                NSCompositingOperation::SourceOver,
                fraction,
            );
        }
    });
    with_tile(4, "part of an image", |r| {
        card64.drawInRect_fromRect_operation_fraction(
            inset(r, 8.0),
            rect(16.0, 16.0, 24.0, 24.0),
            NSCompositingOperation::SourceOver,
            1.0,
        );
    });
    with_tile(5, "rotated", |r| {
        let t = NSAffineTransform::transform();
        let c = center(r);
        t.translateXBy_yBy(c.x, c.y);
        t.rotateByDegrees(30.0);
        t.concat();
        card64.drawInRect(rect(-40.0, -40.0, 80.0, 80.0));
    });
}

/// `image` (a template) in `color`: drawn, then colored where it has ink.
fn tinted(image: Retained<NSImage>, color: Retained<NSColor>) -> Retained<NSImage> {
    let size = image.size();
    let handler = RcBlock::new(move |r: NSRect| -> Bool {
        image.drawInRect(r);
        color.setFill();
        NSRectFillUsingOperation(r, NSCompositingOperation::SourceAtop);
        Bool::YES
    });
    NSImage::imageWithSize_flipped_drawingHandler(size, false, &handler)
}

const SYMBOL_NAMES: &[&str] = &[
    "plus",
    "minus",
    "xmark",
    "checkmark",
    "chevron.left",
    "chevron.right",
    "chevron.up",
    "chevron.down",
    "chevron.up.chevron.down",
    "arrow.left",
    "arrow.right",
    "arrow.up",
    "arrow.down",
    "arrow.up.arrow.down",
    "arrow.down.to.line",
    "arrow.clockwise",
    "arrow.counterclockwise",
    "magnifyingglass",
    "star",
    "star.fill",
    "heart",
    "heart.fill",
    "circle",
    "circle.fill",
    "square",
    "square.fill",
    "info.circle",
    "exclamationmark.circle",
    "questionmark.circle",
    "exclamationmark.triangle",
    "checkmark.circle",
    "checkmark.circle.fill",
    "xmark.circle",
    "xmark.circle.fill",
    "plus.circle",
    "plus.circle.fill",
    "minus.circle",
    "arrow.up.circle",
    "arrow.up.circle.fill",
    "arrow.down.circle",
    "arrow.down.circle.fill",
    "stop.circle.fill",
    "ellipsis.circle",
    "clock",
    "globe",
    "play",
    "play.fill",
    "pause.fill",
    "stop.fill",
    "trash",
    "gearshape",
    "gearshape.fill",
    "folder",
    "folder.fill",
    "doc",
    "doc.fill",
    "doc.on.doc",
    "square.and.arrow.up",
    "square.and.pencil",
    "pencil",
    "paperplane",
    "paperplane.fill",
    "lock",
    "lock.fill",
    "lock.open",
    "person",
    "person.fill",
    "house",
    "bell",
    "bolt",
    "bolt.fill",
    "calendar",
    "bubble.left",
    "bubble.left.fill",
    "photo",
    "terminal",
    "list.bullet",
    "line.3.horizontal",
    "ellipsis",
    "sidebar.left",
    "sidebar.right",
    "slider.horizontal.3",
    "link",
    "square.grid.2x2",
    "eye",
    "eye.slash",
    "tray",
    "bookmark",
    "bookmark.fill",
    "tag",
    "flag",
    "flag.fill",
    "cloud",
    "mic",
    "speaker.wave.2",
];

fn symbols() {
    let config = objc2_app_kit::NSImageSymbolConfiguration::configurationWithPointSize_weight(20.0, 0.0);
    for (i, name) in SYMBOL_NAMES.iter().enumerate() {
        let (col, row) = (i % 16, i / 16);
        let at = NSPoint::new(GAP + col as f64 * 61.0, GAP + row as f64 * 60.0);
        let Some(symbol) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)
        else {
            label("?", at);
            continue;
        };
        let symbol = symbol.imageWithSymbolConfiguration(&config).unwrap_or(symbol);
        let size = symbol.size();
        tinted(symbol, NSColor::labelColor()).drawInRect(rect(
            at.x + (40.0 - size.width) / 2.0,
            at.y + (32.0 - size.height) / 2.0,
            size.width,
            size.height,
        ));
        let short: String = name.chars().take(11).collect();
        let font = NSFont::systemFontOfSize(8.0);
        let color = NSColor::secondaryLabelColor();
        // SAFETY: constant keys.
        let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
        let values: [&AnyObject; 2] = [&font, &color];
        let attrs = NSDictionary::from_slices(&keys, &values);
        // SAFETY: the attributes hold a font and a color.
        unsafe { NSString::from_str(&short).drawAtPoint_withAttributes(NSPoint::new(at.x, at.y + 36.0), Some(&attrs)) };
    }
}

// Views for the opacity scenario: a rounded square of one color.
define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GallerySwatch"]
    #[ivars = Retained<NSColor>]
    struct Swatch;

    impl Swatch {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.ivars().setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), 12.0, 12.0).fill();
        }
    }
);

impl Swatch {
    fn new(mtm: MainThreadMarker, frame: NSRect, color: Retained<NSColor>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(color);
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

#[derive(Default)]
struct CanvasIvars {
    scenario: String,
    /// Seconds spent in the scenario's drawing, and frames drawn.
    drawing: Cell<f64>,
    frames: Cell<u32>,
}

define_class!(
    /// The window's content: draws the scenario.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GalleryCanvas"]
    #[ivars = CanvasIvars]
    struct Canvas;

    impl Canvas {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            NSColor::windowBackgroundColor().setFill();
            NSRectFill(dirty);
            let start = Instant::now();
            match self.ivars().scenario.as_str() {
                "colors" => colors(),
                "images" => images(),
                "symbols" => symbols(),
                "alpha" => label("alpha 1, 0.6, 0.3 (through animator); 0.5 inside 0.5; 0 (not drawn)", NSPoint::new(GAP, GAP)),
                "bench" => bench_frame(),
                "cg" => coregraphics(),
                "cgbench" => cg_bench_frame(),
                "ct" => coretext(),
                _ => shapes(),
            }
            let i = self.ivars();
            i.drawing.set(i.drawing.get() + start.elapsed().as_secs_f64());
            i.frames.set(i.frames.get() + 1);
        }
    }
);

impl Canvas {
    fn new(mtm: MainThreadMarker, frame: NSRect, scenario: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CanvasIvars { scenario: scenario.into(), ..Default::default() });
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// Average milliseconds of drawing a frame since the last call.
    fn take_average(&self) -> f64 {
        let i = self.ivars();
        i.drawing.replace(0.0) * 1000.0 / f64::from(i.frames.replace(0).max(1))
    }
}

// CoreText: lines, glyphs and frames through the view's CGContext.

/// DejaVu Sans at `size`, from the file the conformance tests use.
fn dejavu(size: f64) -> CFRetained<objc2_core_text::CTFont> {
    use objc2_core_graphics::{CGDataProvider, CGFont};
    let data =
        objc2_core_foundation::CFData::from_bytes(include_bytes!("../../../conformance/tests/fixtures/DejaVuSans.ttf"));
    let provider = CGDataProvider::with_cf_data(Some(&data)).expect("a provider");
    let file = CGFont::with_data_provider(&provider).expect("a font");
    // SAFETY: no matrix or attributes.
    unsafe { objc2_core_text::CTFont::with_graphics_font(&file, size, std::ptr::null(), None) }
}

/// An attributed string of `text` in `font` with more attributes.
fn ct_string(
    text: &str,
    font: &objc2_core_text::CTFont,
    more: &[(&objc2_core_foundation::CFString, &AnyObject)],
) -> Retained<objc2_foundation::NSAttributedString> {
    let as_ns = |k: &objc2_core_foundation::CFString| -> *const NSString {
        (k as *const objc2_core_foundation::CFString).cast()
    };
    // SAFETY: CFStrings are NSStrings; a font is an object.
    let mut keys: Vec<&NSString> = vec![unsafe { &*as_ns(objc2_core_text::kCTFontAttributeName) }];
    let mut values: Vec<&AnyObject> = vec![unsafe { &*(font as *const objc2_core_text::CTFont).cast() }];
    for (k, v) in more {
        keys.push(unsafe { &*as_ns(k) });
        values.push(v);
    }
    let dict = NSDictionary::from_slices(&keys, &values);
    // SAFETY: a string and its attributes.
    unsafe {
        objc2_foundation::NSAttributedString::initWithString_attributes(
            objc2_foundation::NSAttributedString::alloc(),
            &NSString::from_str(text),
            Some(&dict),
        )
    }
}

fn ct_line(string: &objc2_foundation::NSAttributedString) -> CFRetained<objc2_core_text::CTLine> {
    // SAFETY: an NSAttributedString is a CFAttributedString.
    unsafe {
        objc2_core_text::CTLine::with_attributed_string(
            &*(string as *const objc2_foundation::NSAttributedString).cast(),
        )
    }
}

/// Draw `line` with its baseline at `at` in the (flipped) view.
fn draw_ct_line(c: &CGContext, line: &objc2_core_text::CTLine, at: NSPoint) {
    CGContext::set_text_position(Some(c), at.x, at.y);
    // SAFETY: a line and a context.
    unsafe { line.draw(c) };
}

fn coretext() {
    use objc2_core_text::*;
    let c = cg();
    // The view is flipped: the text matrix turns text back up.
    let upright = CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: -1.0, tx: 0.0, ty: 0.0 };
    CGContext::set_text_matrix(Some(&c), upright);
    let red = CGColor::new_srgb(0.88, 0.11, 0.14, 1.0);
    let ink = NSColor::labelColor();
    let ink_key: &objc2_core_foundation::CFString =
        unsafe { &*(NSForegroundColorAttributeName as *const NSString).cast() };
    with_tile(0, "CTLine, three sizes", |r| {
        for (k, size) in [11.0, 16.0, 24.0].into_iter().enumerate() {
            let line = ct_line(&ct_string("Hxg fi AV", &dejavu(size), &[(ink_key, &ink)]));
            draw_ct_line(&c, &line, at(r, 8.0, 30.0 + 45.0 * k as f64));
        }
    });
    with_tile(1, "colors, kerning, underline", |r| {
        let three = NSNumber::new_f64(3.0);
        let one = NSNumber::new_i64(1);
        let font = dejavu(16.0);
        // SAFETY: the constants are strings.
        let rows: [Vec<(&objc2_core_foundation::CFString, &AnyObject)>; 3] = unsafe {
            [
                vec![(kCTForegroundColorAttributeName, &*(&*red as *const CGColor).cast::<AnyObject>())],
                vec![(kCTKernAttributeName, &*three), (ink_key, &ink)],
                vec![(kCTUnderlineStyleAttributeName, &*one), (ink_key, &ink)],
            ]
        };
        for (k, extra) in rows.iter().enumerate() {
            draw_ct_line(&c, &ct_line(&ct_string("Kerning AV", &font, extra)), at(r, 8.0, 35.0 + 40.0 * k as f64));
        }
    });
    with_tile(2, "glyphs by position", |r| {
        let font = dejavu(20.0);
        let text: Vec<u16> = "Sidestep".encode_utf16().collect();
        let mut glyphs = vec![0u16; text.len()];
        // SAFETY: as many glyphs as characters.
        unsafe {
            font.glyphs_for_characters(
                NonNull::new(text.as_ptr().cast_mut()).unwrap(),
                NonNull::new(glyphs.as_mut_ptr()).unwrap(),
                text.len() as isize,
            )
        };
        // Positions are in text space, through the text matrix: from the
        // text position, y up.
        CGContext::set_text_position(Some(&c), r.origin.x + 10.0, r.origin.y + 80.0);
        let positions: Vec<CGPoint> =
            (0..glyphs.len()).map(|i| CGPoint::new(16.0 * i as f64, 20.0 * (i as f64 * 0.8).sin())).collect();
        CGContext::set_fill_color_with_color(Some(&c), Some(&NSColor::systemBlueColor().CGColor()));
        // SAFETY: as many positions as glyphs.
        unsafe {
            font.draw_glyphs(
                NonNull::new(glyphs.as_ptr().cast_mut()).unwrap(),
                NonNull::new(positions.as_ptr().cast_mut()).unwrap(),
                glyphs.len(),
                &c,
            )
        };
    });
    with_tile(3, "turned (outlines)", |r| {
        let line = ct_line(&ct_string("Turned text", &dejavu(18.0), &[(ink_key, &ink)]));
        let middle = center(r);
        CGContext::translate_ctm(Some(&c), middle.x, middle.y);
        CGContext::rotate_ctm(Some(&c), -0.6);
        draw_ct_line(&c, &line, NSPoint::new(-50.0, 6.0));
    });
    with_tile(4, "stroke and fill-stroke", |r| {
        let line = ct_line(&ct_string("Hollow", &dejavu(28.0), &[]));
        CGContext::set_stroke_color_with_color(Some(&c), Some(&NSColor::systemGreenColor().CGColor()));
        CGContext::set_line_width(Some(&c), 1.0);
        CGContext::set_text_drawing_mode(Some(&c), CGTextDrawingMode::Stroke);
        draw_ct_line(&c, &line, at(r, 10.0, 60.0));
        CGContext::set_fill_color_with_color(Some(&c), Some(&red));
        CGContext::set_text_drawing_mode(Some(&c), CGTextDrawingMode::FillStroke);
        let filled = ct_line(&ct_string("Filled", &dejavu(28.0), &[(from_context_key(), &*NSNumber::new_bool(true))]));
        draw_ct_line(&c, &filled, at(r, 10.0, 110.0));
        CGContext::set_text_drawing_mode(Some(&c), CGTextDrawingMode::Fill);
    });
    with_tile(5, "truncated", |r| {
        let font = dejavu(14.0);
        let long = ct_line(&ct_string("Hello wonderful world", &font, &[(ink_key, &ink)]));
        let token = ct_line(&ct_string("\u{2026}", &font, &[(ink_key, &ink)]));
        for (k, kind) in [CTLineTruncationType::End, CTLineTruncationType::Middle, CTLineTruncationType::Start]
            .into_iter()
            .enumerate()
        {
            // SAFETY: lines.
            let cut = unsafe { long.truncated_line(r.size.width - 16.0, kind, Some(&token)) }.expect("a line");
            draw_ct_line(&c, &cut, at(r, 8.0, 35.0 + 40.0 * k as f64));
        }
    });
    with_tile(6, "a frame, centered", |r| {
        let align = CTTextAlignment::Center;
        let setting = CTParagraphStyleSetting {
            spec: CTParagraphStyleSpecifier::Alignment,
            valueSize: std::mem::size_of::<CTTextAlignment>(),
            value: NonNull::from(&align).cast(),
        };
        // SAFETY: one setting.
        let style = unsafe { CTParagraphStyle::new(&setting, 1) };
        let text = ct_string(
            "A framesetter fills a rectangle with lines, centered by a paragraph style.",
            &dejavu(12.0),
            // SAFETY: the constant is a string.
            &[
                (unsafe { kCTParagraphStyleAttributeName }, unsafe { &*(&*style as *const CTParagraphStyle).cast() }),
                (ink_key, &ink),
            ],
        );
        // SAFETY: an attributed string, a path.
        unsafe {
            let fs = CTFramesetter::with_attributed_string(&*(Retained::as_ptr(&text)).cast());
            // Frames lay out upward from the bottom: draw in an unflipped
            // space over the tile.
            CGContext::translate_ctm(Some(&c), r.origin.x, r.origin.y + r.size.height);
            CGContext::scale_ctm(Some(&c), 1.0, -1.0);
            CGContext::set_text_matrix(
                Some(&c),
                CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 },
            );
            let path =
                CGPath::with_rect(cgr(rect(8.0, 8.0, r.size.width - 16.0, r.size.height - 16.0)), std::ptr::null());
            fs.frame(objc2_core_foundation::CFRange::new(0, 0), &path, None).draw(&c);
        }
        CGContext::set_text_matrix(Some(&c), upright);
    });
}

/// `kCTForegroundColorFromContextAttributeName`.
fn from_context_key() -> &'static objc2_core_foundation::CFString {
    // SAFETY: the constant is a string.
    unsafe { objc2_core_text::kCTForegroundColorFromContextAttributeName }
}

// CoreGraphics: the same kinds of drawing through a CGContext.

/// The current context's CGContext.
fn cg() -> Retained<CGContext> {
    NSGraphicsContext::currentContext().expect("a context in drawRect:").CGContext()
}

fn cgr(r: NSRect) -> CGRect {
    CGRect::new(CGPoint::new(r.origin.x, r.origin.y), CGSize::new(r.size.width, r.size.height))
}

fn cg_srgb() -> CFRetained<CGColorSpace> {
    CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).expect("sRGB")
}

/// A `w` × `h` CGImage drawn by `f` into a BGRA bitmap context (the byte
/// order Macs draw in), user space's origin at its bottom left.
fn cg_image(w: usize, h: usize, f: impl FnOnce(&CGContext)) -> CFRetained<CGImage> {
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    // SAFETY: no data: the context allocates.
    let c =
        unsafe { CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, Some(&cg_srgb()), info) }.expect("a context");
    f(&c);
    CGBitmapContextCreateImage(Some(&c)).expect("an image")
}

/// Draw `image` into `r` of a flipped view the right way up.
fn draw_upright(c: &CGContext, r: NSRect, image: &CGImage) {
    CGContext::save_g_state(Some(c));
    CGContext::translate_ctm(Some(c), r.origin.x, r.origin.y + r.size.height);
    CGContext::scale_ctm(Some(c), 1.0, -1.0);
    CGContext::draw_image(Some(c), CGRect::new(CGPoint::ZERO, CGSize::new(r.size.width, r.size.height)), Some(image));
    CGContext::restore_g_state(Some(c));
}

fn gradient(stops: &[[f64; 4]]) -> CFRetained<CGGradient> {
    let comps: Vec<f64> = stops.iter().flatten().copied().collect();
    // SAFETY: four components a stop; no locations.
    unsafe { CGGradient::with_color_components(Some(&cg_srgb()), comps.as_ptr(), std::ptr::null(), stops.len()) }
        .expect("a gradient")
}

fn coregraphics() {
    let c = cg();
    let red = CGColor::new_srgb(0.88, 0.11, 0.14, 1.0);
    let blue = CGColor::new_srgb(0.21, 0.52, 0.89, 1.0);
    let green = CGColor::new_srgb(0.18, 0.76, 0.49, 1.0);
    let ink = NSColor::labelColor().CGColor();
    with_tile(0, "fills", |r| {
        CGContext::set_fill_color_with_color(Some(&c), Some(&red));
        CGContext::fill_rect(Some(&c), cgr(rect(r.origin.x + 10.0, r.origin.y + 10.0, 60.0, 60.0)));
        CGContext::set_rgb_fill_color(Some(&c), 0.21, 0.52, 0.89, 0.6);
        CGContext::fill_rect(Some(&c), cgr(rect(r.origin.x + 40.0, r.origin.y + 40.0, 70.0, 70.0)));
        CGContext::set_fill_color_with_color(Some(&c), Some(&green));
        CGContext::fill_ellipse_in_rect(Some(&c), cgr(rect(r.origin.x + 80.0, r.origin.y + 80.0, 60.0, 60.0)));
    });
    with_tile(1, "rounded CGPaths", |r| {
        CGContext::set_fill_color_with_color(Some(&c), Some(&NSColor::systemBlueColor().CGColor()));
        for (k, radius) in [2.0, 8.0, 15.0].into_iter().enumerate() {
            let y = r.origin.y + 8.0 + k as f64 * 35.0;
            // SAFETY: no transform.
            let p = unsafe {
                CGPath::with_rounded_rect(cgr(rect(r.origin.x + 8.0, y, 134.0, 30.0)), radius, radius, std::ptr::null())
            };
            CGContext::add_path(Some(&c), Some(&p));
            CGContext::fill_path(Some(&c));
        }
        let p = CGMutablePath::new();
        // SAFETY: no transform.
        unsafe {
            CGMutablePath::add_rounded_rect(
                Some(&p),
                std::ptr::null(),
                cgr(rect(r.origin.x + 8.0, r.origin.y + 113.0, 134.0, 30.0)),
                15.0,
                15.0,
            )
        };
        CGContext::set_stroke_color_with_color(Some(&c), Some(&ink));
        CGContext::set_line_width(Some(&c), 2.0);
        CGContext::add_path(Some(&c), Some(&p));
        CGContext::stroke_path(Some(&c));
    });
    with_tile(2, "caps, joins, dashes", |r| {
        CGContext::set_stroke_color_with_color(Some(&c), Some(&ink));
        let styles = [
            (CGLineCap::Butt, CGLineJoin::Miter),
            (CGLineCap::Round, CGLineJoin::Round),
            (CGLineCap::Square, CGLineJoin::Bevel),
        ];
        CGContext::set_line_width(Some(&c), 9.0);
        for (k, (cap, join)) in styles.into_iter().enumerate() {
            let x = r.origin.x + 20.0 + k as f64 * 45.0;
            CGContext::set_line_cap(Some(&c), cap);
            CGContext::set_line_join(Some(&c), join);
            CGContext::move_to_point(Some(&c), x, r.origin.y + 20.0);
            CGContext::add_line_to_point(Some(&c), x + 25.0, r.origin.y + 60.0);
            CGContext::add_line_to_point(Some(&c), x, r.origin.y + 100.0);
            CGContext::stroke_path(Some(&c));
        }
        CGContext::set_line_width(Some(&c), 3.0);
        CGContext::set_line_cap(Some(&c), CGLineCap::Butt);
        for (k, pattern) in [[6.0, 3.0], [2.0, 4.0]].iter().enumerate() {
            // SAFETY: two lengths.
            unsafe { CGContext::set_line_dash(Some(&c), 0.0, pattern.as_ptr(), 2) };
            let y = r.origin.y + 120.0 + k as f64 * 14.0;
            CGContext::move_to_point(Some(&c), r.origin.x + 10.0, y);
            CGContext::add_line_to_point(Some(&c), r.origin.x + 140.0, y);
            CGContext::stroke_path(Some(&c));
        }
    });
    with_tile(3, "arcs and curves", |r| {
        let (cx, cy) = (r.origin.x + 45.0, r.origin.y + 45.0);
        CGContext::set_fill_color_with_color(Some(&c), Some(&red));
        CGContext::set_stroke_color_with_color(Some(&c), Some(&ink));
        CGContext::set_line_width(Some(&c), 2.0);
        CGContext::move_to_point(Some(&c), cx, cy);
        CGContext::add_arc(Some(&c), cx, cy, 35.0, 0.3, 5.2, 0);
        CGContext::close_path(Some(&c));
        CGContext::draw_path(Some(&c), CGPathDrawingMode::FillStroke);
        CGContext::move_to_point(Some(&c), r.origin.x + 90.0, r.origin.y + 10.0);
        CGContext::add_arc_to_point(
            Some(&c),
            r.origin.x + 140.0,
            r.origin.y + 10.0,
            r.origin.x + 140.0,
            r.origin.y + 80.0,
            25.0,
        );
        CGContext::add_line_to_point(Some(&c), r.origin.x + 140.0, r.origin.y + 80.0);
        CGContext::stroke_path(Some(&c));
        CGContext::set_stroke_color_with_color(Some(&c), Some(&blue));
        CGContext::set_line_width(Some(&c), 3.0);
        CGContext::move_to_point(Some(&c), r.origin.x + 10.0, r.origin.y + 140.0);
        CGContext::add_curve_to_point(
            Some(&c),
            r.origin.x + 40.0,
            r.origin.y + 60.0,
            r.origin.x + 110.0,
            r.origin.y + 150.0,
            r.origin.x + 140.0,
            r.origin.y + 100.0,
        );
        CGContext::add_quad_curve_to_point(
            Some(&c),
            r.origin.x + 100.0,
            r.origin.y + 80.0,
            r.origin.x + 60.0,
            r.origin.y + 100.0,
        );
        CGContext::stroke_path(Some(&c));
    });
    with_tile(4, "gradients", |r| {
        let g = gradient(&[[0.95, 0.3, 0.2, 1.0], [0.95, 0.8, 0.2, 1.0], [0.2, 0.6, 0.95, 1.0]]);
        CGContext::save_g_state(Some(&c));
        CGContext::clip_to_rect(Some(&c), cgr(rect(r.origin.x + 8.0, r.origin.y + 8.0, 134.0, 60.0)));
        CGContext::draw_linear_gradient(
            Some(&c),
            Some(&g),
            CGPoint::new(r.origin.x + 8.0, 0.0),
            CGPoint::new(r.origin.x + 142.0, 0.0),
            CGGradientDrawingOptions::empty(),
        );
        CGContext::restore_g_state(Some(&c));
        let both = CGGradientDrawingOptions::DrawsBeforeStartLocation | CGGradientDrawingOptions::DrawsAfterEndLocation;
        CGContext::save_g_state(Some(&c));
        CGContext::clip_to_rect(Some(&c), cgr(rect(r.origin.x + 8.0, r.origin.y + 76.0, 134.0, 66.0)));
        CGContext::draw_radial_gradient(
            Some(&c),
            Some(&g),
            CGPoint::new(r.origin.x + 60.0, r.origin.y + 100.0),
            4.0,
            CGPoint::new(r.origin.x + 75.0, r.origin.y + 109.0),
            50.0,
            both,
        );
        CGContext::restore_g_state(Some(&c));
    });
    with_tile(5, "shadows and layers", |r| {
        let shadow = CGColor::new_generic_gray(0.0, 0.6);
        CGContext::save_g_state(Some(&c));
        CGContext::set_shadow_with_color(Some(&c), CGSize::new(4.0, -4.0), 4.0, Some(&shadow));
        CGContext::set_fill_color_with_color(Some(&c), Some(&blue));
        CGContext::fill_rect(Some(&c), cgr(rect(r.origin.x + 12.0, r.origin.y + 12.0, 50.0, 50.0)));
        CGContext::restore_g_state(Some(&c));
        // Two overlapping squares faded as one, with one shadow.
        CGContext::save_g_state(Some(&c));
        CGContext::set_alpha(Some(&c), 0.6);
        CGContext::set_shadow_with_color(Some(&c), CGSize::new(3.0, -3.0), 3.0, Some(&shadow));
        // SAFETY: no auxiliary info.
        unsafe { CGContext::begin_transparency_layer(Some(&c), None) };
        CGContext::set_fill_color_with_color(Some(&c), Some(&red));
        CGContext::fill_rect(Some(&c), cgr(rect(r.origin.x + 70.0, r.origin.y + 60.0, 50.0, 50.0)));
        CGContext::set_fill_color_with_color(Some(&c), Some(&green));
        CGContext::fill_rect(Some(&c), cgr(rect(r.origin.x + 90.0, r.origin.y + 85.0, 50.0, 50.0)));
        CGContext::end_transparency_layer(Some(&c));
        CGContext::restore_g_state(Some(&c));
    });
    with_tile(6, "clips", |r| {
        CGContext::save_g_state(Some(&c));
        CGContext::add_ellipse_in_rect(Some(&c), cgr(rect(r.origin.x + 8.0, r.origin.y + 8.0, 134.0, 70.0)));
        CGContext::add_ellipse_in_rect(Some(&c), cgr(rect(r.origin.x + 48.0, r.origin.y + 28.0, 54.0, 30.0)));
        CGContext::eo_clip(Some(&c));
        for k in 0..10 {
            let color = if k % 2 == 0 { &red } else { &blue };
            CGContext::set_fill_color_with_color(Some(&c), Some(color));
            CGContext::fill_rect(Some(&c), cgr(rect(r.origin.x + k as f64 * 15.0, r.origin.y, 15.0, 90.0)));
        }
        CGContext::restore_g_state(Some(&c));
        // A clip to a mask: a gray image, drawn with a radial gradient.
        let gray = CGColorSpace::new_device_gray().expect("gray");
        // SAFETY: no data: the context allocates.
        let m =
            unsafe { CGBitmapContextCreate(std::ptr::null_mut(), 64, 64, 8, 0, Some(&gray), CGImageAlphaInfo::None.0) }
                .expect("a gray context");
        let g = gradient(&[[1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]]);
        let after = CGGradientDrawingOptions::DrawsAfterEndLocation;
        CGContext::draw_radial_gradient(
            Some(&m),
            Some(&g),
            CGPoint::new(32.0, 32.0),
            0.0,
            CGPoint::new(32.0, 32.0),
            32.0,
            after,
        );
        let mask = CGBitmapContextCreateImage(Some(&m)).expect("a mask");
        CGContext::save_g_state(Some(&c));
        let area = cgr(rect(r.origin.x + 40.0, r.origin.y + 90.0, 70.0, 56.0));
        CGContext::clip_to_mask(Some(&c), area, Some(&mask));
        CGContext::set_fill_color_with_color(Some(&c), Some(&green));
        CGContext::fill_rect(Some(&c), area);
        CGContext::restore_g_state(Some(&c));
    });
    with_tile(7, "images from bitmap contexts", |r| {
        let image = cg_image(48, 48, |m| {
            CGContext::set_rgb_fill_color(Some(m), 0.95, 0.95, 0.95, 1.0);
            CGContext::fill_rect(Some(m), CGRect::new(CGPoint::ZERO, CGSize::new(48.0, 48.0)));
            CGContext::set_rgb_fill_color(Some(m), 0.88, 0.11, 0.14, 1.0);
            CGContext::fill_rect(Some(m), CGRect::new(CGPoint::ZERO, CGSize::new(24.0, 24.0)));
            CGContext::set_rgb_fill_color(Some(m), 0.21, 0.52, 0.89, 1.0);
            CGContext::fill_ellipse_in_rect(Some(m), CGRect::new(CGPoint::new(20.0, 20.0), CGSize::new(26.0, 26.0)));
        });
        draw_upright(&c, rect(r.origin.x + 8.0, r.origin.y + 8.0, 48.0, 48.0), &image);
        draw_upright(&c, rect(r.origin.x + 64.0, r.origin.y + 8.0, 78.0, 96.0), &image);
        // Upside down, as CGContextDrawImage draws in a flipped view.
        CGContext::draw_image(Some(&c), cgr(rect(r.origin.x + 8.0, r.origin.y + 64.0, 48.0, 48.0)), Some(&image));
        let part =
            CGImage::with_image_in_rect(Some(&image), CGRect::new(CGPoint::new(0.0, 24.0), CGSize::new(24.0, 24.0)));
        if let Some(part) = part {
            draw_upright(&c, rect(r.origin.x + 8.0, r.origin.y + 118.0, 24.0, 24.0), &part);
        }
    });
    with_tile(8, "blend modes", |r| {
        let modes = [
            CGBlendMode::Normal,
            CGBlendMode::Multiply,
            CGBlendMode::Screen,
            CGBlendMode::Difference,
            CGBlendMode::XOR,
            CGBlendMode::PlusLighter,
        ];
        for (k, mode) in modes.into_iter().enumerate() {
            let (x, y) = (r.origin.x + 6.0 + (k % 2) as f64 * 72.0, r.origin.y + 6.0 + (k / 2) as f64 * 48.0);
            CGContext::save_g_state(Some(&c));
            CGContext::set_fill_color_with_color(Some(&c), Some(&blue));
            CGContext::fill_rect(Some(&c), cgr(rect(x, y, 40.0, 40.0)));
            CGContext::set_blend_mode(Some(&c), mode);
            CGContext::set_rgb_fill_color(Some(&c), 0.95, 0.6, 0.1, 1.0);
            CGContext::fill_ellipse_in_rect(Some(&c), cgr(rect(x + 20.0, y + 10.0, 40.0, 36.0)));
            CGContext::restore_g_state(Some(&c));
        }
    });
    with_tile(9, "transforms", |r| {
        CGContext::save_g_state(Some(&c));
        CGContext::translate_ctm(Some(&c), r.origin.x + 75.0, r.origin.y + 75.0);
        for k in 0..12 {
            CGContext::rotate_ctm(Some(&c), std::f64::consts::TAU / 12.0);
            let t = k as f64 / 12.0;
            CGContext::set_rgb_fill_color(Some(&c), 0.2 + 0.7 * t, 0.4, 0.9 - 0.6 * t, 0.85);
            CGContext::fill_rect(Some(&c), CGRect::new(CGPoint::new(20.0, -5.0), CGSize::new(45.0, 10.0)));
        }
        CGContext::scale_ctm(Some(&c), 1.5, 0.7);
        CGContext::set_fill_color_with_color(Some(&c), Some(&ink));
        CGContext::fill_ellipse_in_rect(Some(&c), CGRect::new(CGPoint::new(-10.0, -10.0), CGSize::new(20.0, 20.0)));
        CGContext::restore_g_state(Some(&c));
    });
    with_tile(10, "system colors as CGColors", |r| {
        let named = [
            NSColor::systemRedColor,
            NSColor::systemOrangeColor,
            NSColor::systemYellowColor,
            NSColor::systemGreenColor,
            NSColor::systemTealColor,
            NSColor::systemBlueColor,
            NSColor::systemIndigoColor,
            NSColor::systemPurpleColor,
            NSColor::systemPinkColor,
        ];
        for (k, make) in named.iter().enumerate() {
            let (x, y) = (r.origin.x + 8.0 + (k % 3) as f64 * 46.0, r.origin.y + 8.0 + (k / 3) as f64 * 46.0);
            CGContext::set_fill_color_with_color(Some(&c), Some(&make().CGColor()));
            // SAFETY: no transform.
            let p = unsafe { CGPath::with_rounded_rect(cgr(rect(x, y, 40.0, 40.0)), 8.0, 8.0, std::ptr::null()) };
            CGContext::add_path(Some(&c), Some(&p));
            CGContext::fill_path(Some(&c));
        }
    });
    with_tile(11, "stroked and dashed CGPaths", |r| {
        // SAFETY: no transform.
        let star = unsafe {
            let p = CGMutablePath::new();
            for k in 0..10 {
                let a = std::f64::consts::FRAC_PI_2 + k as f64 * std::f64::consts::PI / 5.0;
                let radius = if k % 2 == 0 { 40.0 } else { 16.0 };
                let (x, y) = (r.origin.x + 75.0 + radius * a.cos(), r.origin.y + 50.0 - radius * a.sin());
                if k == 0 {
                    CGMutablePath::move_to_point(Some(&p), std::ptr::null(), x, y)
                } else {
                    CGMutablePath::add_line_to_point(Some(&p), std::ptr::null(), x, y)
                }
            }
            CGMutablePath::close_subpath(Some(&p));
            p
        };
        // SAFETY: no transform.
        let outline = unsafe {
            CGPath::new_copy_by_stroking_path(
                Some(&star),
                std::ptr::null(),
                8.0,
                CGLineCap::Round,
                CGLineJoin::Round,
                10.0,
            )
        };
        if let Some(outline) = outline {
            CGContext::set_fill_color_with_color(Some(&c), Some(&blue));
            CGContext::add_path(Some(&c), Some(&outline));
            CGContext::fill_path(Some(&c));
        }
        let lengths = [8.0, 4.0];
        // SAFETY: no transform; two lengths.
        let dashed = unsafe {
            CGPath::new_copy_by_dashing_path(
                Some(&CGPath::with_ellipse_in_rect(
                    cgr(rect(r.origin.x + 20.0, r.origin.y + 100.0, 110.0, 44.0)),
                    std::ptr::null(),
                )),
                std::ptr::null(),
                0.0,
                lengths.as_ptr(),
                2,
            )
        };
        if let Some(dashed) = dashed {
            CGContext::set_stroke_color_with_color(Some(&c), Some(&ink));
            CGContext::set_line_width(Some(&c), 2.0);
            CGContext::add_path(Some(&c), Some(&dashed));
            CGContext::stroke_path(Some(&c));
        }
    });
}

thread_local!(static CG_BENCH_IMAGES: OnceCell<(Retained<CGImage>, Retained<CGImage>)> = const { OnceCell::new() });

/// A checkerboard `n` pixels square as a CGImage.
fn card_image(n: usize) -> Retained<CGImage> {
    let image = card(n);
    // SAFETY: no proposed rectangle, context or hints.
    unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }.expect("a CGImage")
}

/// The bench frame through a CGContext: the same 2,000 rounded-rect fills
/// (a CGPath each, added and filled), 1,000 strokes and 300 images.
fn cg_bench_frame() {
    let c = cg();
    let (small, big) = CG_BENCH_IMAGES.with(|i| i.get_or_init(|| (card_image(32), card_image(256))).clone());
    let colors = [
        CGColor::new_srgb(0.88, 0.11, 0.14, 1.0),
        CGColor::new_srgb(0.21, 0.52, 0.89, 1.0),
        CGColor::new_srgb(0.18, 0.76, 0.49, 0.8),
    ];
    for k in 0..2000 {
        let (x, y) = ((k % 50) as f64 * 19.0 + 4.0, (k / 50) as f64 * 9.0 + 4.0);
        CGContext::set_fill_color_with_color(Some(&c), Some(&colors[k % 3]));
        // SAFETY: no transform.
        let p = unsafe { CGPath::with_rounded_rect(cgr(rect(x, y, 16.0, 7.0)), 3.0, 3.0, std::ptr::null()) };
        CGContext::add_path(Some(&c), Some(&p));
        CGContext::fill_path(Some(&c));
    }
    CGContext::set_stroke_color_with_color(Some(&c), Some(&NSColor::labelColor().CGColor()));
    CGContext::set_line_width(Some(&c), 1.0);
    for k in 0..1000 {
        let (x, y) = ((k % 40) as f64 * 24.0 + 2.0, (k / 40) as f64 * 14.0 + 2.0);
        CGContext::move_to_point(Some(&c), x, y);
        CGContext::add_line_to_point(Some(&c), x + 20.0, y + 12.0);
        CGContext::stroke_path(Some(&c));
    }
    for k in 0..300 {
        let (x, y) = ((k % 25) as f64 * 38.0 + 4.0, (k / 25) as f64 * 30.0 + 4.0);
        let image = if k % 2 == 0 { &small } else { &big };
        CGContext::draw_image(Some(&c), cgr(rect(x, y, 28.0, 28.0)), Some(image));
    }
}

// The benchmark.

thread_local!(static BENCH_IMAGES: OnceCell<(Retained<NSImage>, Retained<NSImage>)> = const { OnceCell::new() });

/// One frame: 2,000 rounded-rect fills, 1,000 strokes, 300 images (half of
/// them a 256-point image drawn at 32 points).
fn bench_frame() {
    let (small, big) = BENCH_IMAGES.with(|i| i.get_or_init(|| (card(32), card(256))).clone());
    let colors = [srgb(0.88, 0.11, 0.14, 1.0), srgb(0.21, 0.52, 0.89, 1.0), srgb(0.18, 0.76, 0.49, 0.8)];
    for k in 0..2000 {
        let (x, y) = ((k % 50) as f64 * 19.0 + 4.0, (k / 50) as f64 * 9.0 + 4.0);
        colors[k % 3].setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect(x, y, 16.0, 7.0), 3.0, 3.0).fill();
    }
    NSColor::labelColor().setStroke();
    for k in 0..1000 {
        let (x, y) = ((k % 40) as f64 * 24.0 + 2.0, (k / 40) as f64 * 14.0 + 2.0);
        NSBezierPath::strokeLineFromPoint_toPoint(NSPoint::new(x, y), NSPoint::new(x + 20.0, y + 12.0));
    }
    for k in 0..300 {
        let (x, y) = ((k % 25) as f64 * 38.0 + 4.0, (k / 25) as f64 * 30.0 + 4.0);
        let image = if k % 2 == 0 { &small } else { &big };
        image.drawInRect(rect(x, y, 28.0, 28.0));
    }
}

/// Milliseconds to draw `view` into a new bitmap at `scale` pixels a point
/// (recording and rasterizing, on this thread): the median of seven.
fn time_bitmap(view: &NSView, scale: f64) -> f64 {
    let b = view.bounds();
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            // SAFETY: NULL planes make the rep allocate its own.
            let rep = unsafe {
                NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                    NSBitmapImageRep::alloc(),
                    std::ptr::null_mut(),
                    (b.size.width * scale) as isize,
                    (b.size.height * scale) as isize,
                    8,
                    4,
                    true,
                    false,
                    NSDeviceRGBColorSpace,
                    0,
                    32,
                )
            }
            .expect("a bitmap");
            rep.setSize(b.size);
            let start = Instant::now();
            view.cacheDisplayInRect_toBitmapImageRep(b, &rep);
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    runs[3]
}

#[derive(Default)]
struct DelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    timers: RefCell<Vec<Retained<NSTimer>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GalleryDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.open_window();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate(&self, _sender: &NSApplication) -> bool {
            true
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars::default());
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn timer(&self, seconds: f64, repeats: bool, f: impl Fn() + 'static) {
        let block = RcBlock::new(move |_: NonNull<NSTimer>| f());
        // SAFETY: the timer copies the block.
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, repeats, &block) };
        self.ivars().timers.borrow_mut().push(timer);
    }

    fn open_window(&self) {
        let mtm = self.mtm();
        let scenario = std::env::var("SCENARIO").unwrap_or_else(|_| "shapes".into());
        let canvas = content(mtm, &scenario);
        let style_mask = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        // SAFETY: a new window, which we keep.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                canvas.frame(),
                style_mask,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: as above.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Sidestep drawing gallery"));
        window.setContentView(Some(&canvas));

        if scenario == "bench" || scenario == "cgbench" {
            // Redraw every frame for a second, then time drawing into
            // bitmaps at 1× and 2×.
            let redraw = canvas.clone();
            self.timer(1.0 / 60.0, true, move || redraw.setNeedsDisplay(true));
            let canvas = canvas.clone();
            self.timer(1.5, false, move || {
                println!("bench: in the window, {:.2} ms drawing a frame", canvas.take_average());
                for scale in [1.0, 2.0] {
                    let total = time_bitmap(&canvas, scale);
                    println!(
                        "bench at {scale}x: into a bitmap in {total:.2} ms (median of 7), {:.2} ms of it drawing",
                        canvas.take_average()
                    );
                }
                NSApplication::sharedApplication(MainThreadMarker::new().expect("main thread")).terminate(None);
            });
        }

        if let Some(secs) = std::env::var("GALLERY_QUIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()) {
            self.timer(secs, false, || {
                NSApplication::sharedApplication(MainThreadMarker::new().expect("main thread")).terminate(None)
            });
        }
        window.makeKeyAndOrderFront(None);
        let _ = self.ivars().window.set(window);
    }
}

/// The scenario's content view, with its subviews.
fn content(mtm: MainThreadMarker, scenario: &str) -> Retained<Canvas> {
    let frame = rect(0.0, 0.0, COLUMNS as f64 * (TILE + GAP) + GAP, 2.0 * (TILE + 18.0 + GAP) + GAP);
    let canvas = Canvas::new(mtm, frame, scenario);
    if scenario == "alpha" {
        let colors = [NSColor::systemRedColor(), NSColor::systemGreenColor(), NSColor::systemBlueColor()];
        for (k, color) in colors.into_iter().enumerate() {
            let swatch = Swatch::new(mtm, rect(40.0 + k as f64 * 90.0, 60.0, 160.0, 160.0), color);
            canvas.addSubview(&swatch);
            if k == 1 {
                swatch.setAlphaValue(0.6);
            } else if k == 2 {
                let changes = RcBlock::new(move |c: NonNull<NSAnimationContext>| {
                    // SAFETY: AppKit passes the context.
                    unsafe { c.as_ref() }.setDuration(0.0);
                    swatch.animator().setAlphaValue(0.3);
                });
                NSAnimationContext::runAnimationGroup(&changes);
            }
        }
        // A group in a group: the child fades with its parent.
        let outer = Swatch::new(mtm, rect(420.0, 60.0, 180.0, 180.0), NSColor::systemOrangeColor());
        outer.setAlphaValue(0.5);
        let inner = Swatch::new(mtm, rect(40.0, 40.0, 140.0, 140.0), NSColor::systemPurpleColor());
        inner.setAlphaValue(0.5);
        outer.addSubview(&inner);
        canvas.addSubview(&outer);
        let invisible = Swatch::new(mtm, rect(660.0, 60.0, 160.0, 160.0), NSColor::labelColor());
        invisible.setAlphaValue(0.0);
        canvas.addSubview(&invisible);
    }
    canvas
}

/// GALLERY_PNG: draw the scenario into a bitmap and write it to `path`,
/// or time the bench, with no window.
fn headless(mtm: MainThreadMarker, path: &str) {
    let scenario = std::env::var("SCENARIO").unwrap_or_else(|_| "shapes".into());
    let canvas = content(mtm, &scenario);
    if scenario == "bench" || scenario == "cgbench" {
        for scale in [1.0, 2.0] {
            let total = time_bitmap(&canvas, scale);
            println!(
                "bench at {scale}x: into a bitmap in {total:.2} ms (median of 7), {:.2} ms of it drawing",
                canvas.take_average()
            );
        }
        return;
    }
    let scale: f64 = std::env::var("GALLERY_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let b = canvas.bounds();
    let (w, h) = ((b.size.width * scale) as usize, (b.size.height * scale) as usize);
    let rep = bitmap(w, h, |_, _| [0; 4]);
    rep.setSize(b.size);
    canvas.cacheDisplayInRect_toBitmapImageRep(b, &rep);
    // The premultiplied pixels, unpremultiplied for PNG.
    let (data, row) = (rep.bitmapData(), rep.bytesPerRow() as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            // SAFETY: inside the bitmap's rows.
            let [r, g, b, a] = unsafe { std::ptr::read(data.add(y * row + x * 4).cast::<[u8; 4]>()) };
            let un = |c: u8| {
                if a == 0 { 0 } else { ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8 }
            };
            rgba.extend_from_slice(&[un(r), un(g), un(b), a]);
        }
    }
    let image = image::RgbaImage::from_raw(w as u32, h as u32, rgba).expect("pixels");
    image.save(path).expect("the PNG written");
    println!("wrote {path} ({w} x {h})");
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: constant strings.
    let appearance = match std::env::var("GALLERY_APPEARANCE").as_deref() {
        Ok("dark") => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }),
        Ok("light") => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }),
        _ => None,
    };
    if let Some(appearance) = appearance {
        app.setAppearance(Some(&appearance));
    }
    if let Ok(path) = std::env::var("GALLERY_PNG") {
        headless(mtm, &path);
        return;
    }
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
