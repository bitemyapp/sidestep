//! Image views and image cells, checked on macOS and on Linux alike
//! without showing a window: defaults, values, the frame styles' geometry,
//! where an image lands for every scaling and alignment (pixels read back
//! from `cacheDisplayInRect:toBitmapImageRep:`), how template images are
//! tinted, editing, and animated images' frames.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

mod common;

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use common::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{
    AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel,
};
use objc2_app_kit::*;
use objc2_foundation::{NSData, NSDate, NSEdgeInsets, NSNumber, NSPoint, NSRect, NSRunLoop, NSSize, NSString};

use sidestep as _;

type Test = (&'static str, fn(MainThreadMarker));

fn size(w: f64, h: f64) -> NSSize {
    NSSize::new(w, h)
}

/// An image of `w` × `h` points filled with `color` (sRGB components).
fn filled(w: f64, h: f64, color: [f64; 4]) -> Retained<NSImage> {
    let image = NSImage::initWithSize(NSImage::alloc(), size(w, h));
    #[allow(deprecated)]
    image.lockFocus();
    NSColor::colorWithSRGBRed_green_blue_alpha(color[0], color[1], color[2], color[3]).set();
    NSRectFill(rect(0.0, 0.0, w, h));
    #[allow(deprecated)]
    image.unlockFocus();
    image
}

fn red_image(w: f64, h: f64) -> Retained<NSImage> {
    filled(w, h, [1.0, 0.0, 0.0, 1.0])
}

fn image_view(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSImageView> {
    NSImageView::initWithFrame(NSImageView::alloc(mtm), frame)
}

fn aqua() -> Retained<NSAppearance> {
    // SAFETY: the name is AppKit's constant.
    NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }).expect("the aqua appearance")
}

fn is_kind(object: &AnyObject, class: &AnyClass) -> bool {
    // SAFETY: isKindOfClass: takes a class and returns BOOL.
    unsafe { msg_send![object, isKindOfClass: class] }
}

/// The reason an exception gave (a panic's message on Linux).
fn raises(f: impl FnOnce()) -> String {
    use std::panic::AssertUnwindSafe;
    #[cfg(target_vendor = "apple")]
    {
        match objc2::exception::catch(AssertUnwindSafe(f)) {
            Ok(()) => panic!("expected an exception"),
            Err(Some(e)) => {
                // SAFETY: exceptions answer reason with a string.
                let reason: Retained<NSString> = unsafe { msg_send![&*e, reason] };
                reason.to_string()
            }
            Err(None) => panic!("nil exception"),
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let e = std::panic::catch_unwind(AssertUnwindSafe(f)).expect_err("expected a panic");
        match e.downcast::<String>() {
            Ok(s) => *s,
            Err(e) => e.downcast::<&str>().map(|s| s.to_string()).unwrap_or_default(),
        }
    }
}

// What the target saw.
thread_local!(static ACTIONS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn take_actions() -> Vec<String> {
    ACTIONS.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceImageViewTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(changed:))]
        fn changed(&self, sender: &NSImageView) {
            let has = sender.image().is_some();
            ACTIONS.with(|a| a.borrow_mut().push(format!("changed: image {has}")));
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

define_class!(
    #[unsafe(super(NSImageView, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceMyImageView"]
    struct MyImageView;

    unsafe impl NSObjectProtocol for MyImageView {}
);

fn target(mtm: MainThreadMarker) -> Retained<Target> {
    // SAFETY: NSObject's initializer.
    unsafe { msg_send![Target::alloc(mtm), init] }
}

fn insets(view: &NSView) -> (f64, f64, f64, f64) {
    // SAFETY: alignmentRectInsets takes nothing and returns insets.
    let i: NSEdgeInsets = unsafe { msg_send![view, alignmentRectInsets] };
    (i.top, i.left, i.bottom, i.right)
}

fn fits(view: &NSView, proposed: NSSize) -> NSSize {
    // SAFETY: sizeThatFits: takes and returns a size.
    unsafe { msg_send![view, sizeThatFits: proposed] }
}

// Defaults and values

fn image_view_defaults(mtm: MainThreadMarker) {
    let v = NSImageView::new(mtm);
    assert_eq!(v.frame(), NSRect::ZERO);
    let c = v.cell().expect("a cell");
    assert!(is_kind(&c, NSImageCell::class()));
    // SAFETY: +cellClass takes nothing and returns a class.
    let cell_class: Option<&AnyClass> = unsafe { msg_send![NSImageView::class(), cellClass] };
    assert!(cell_class.is_some_and(|k| std::ptr::eq(k, NSImageCell::class())));
    assert!(v.image().is_none());
    assert!(!v.isEditable());
    assert_eq!(v.imageAlignment(), NSImageAlignment::AlignCenter);
    assert_eq!(v.imageScaling(), NSImageScaling::ScaleProportionallyDown);
    assert_eq!(v.imageFrameStyle(), NSImageFrameStyle::None);
    assert!(v.animates() && v.allowsCutCopyPaste());
    assert!(v.contentTintColor().is_none() && v.symbolConfiguration().is_none());
    assert!(v.isEnabled() && !v.isFlipped());
    // It takes the keyboard only while it's editable.
    assert!(!v.acceptsFirstResponder() && v.refusesFirstResponder());
    assert_eq!(v.intrinsicContentSize(), size(0.0, 0.0));
    assert_eq!(insets(&v), (0.0, 0.0, 0.0, 0.0));
    assert_eq!(fits(&v, size(100.0, 50.0)), size(0.0, 0.0));
    // The view keeps the tag, target and action; the cell has none.
    assert_eq!(v.tag(), 0);
    assert!(v.target().is_none() && v.action().is_none());
    assert_eq!((c.tag(), c.action()), (-1, None));
    assert_eq!(c.r#type(), NSCellType::NullCellType);
    assert!(c.isEnabled() && !c.isEditable() && !c.isSelectable() && !c.isBordered() && !c.isBezeled());
    assert!(c.font().is_none());
    assert_eq!(c.sendActionOn(NSEventMask::LeftMouseUp), NSEventMask::LeftMouseUp.0 as isize);
    // It takes image drags whether or not it's editable.
    let types: Vec<String> = v.registeredDraggedTypes().iter().map(|t| t.to_string()).collect();
    for kind in ["NeXT TIFF v4.0 pasteboard type", "Apple PNG pasteboard type", "NSFilenamesPboardType"] {
        assert!(types.iter().any(|t| t == kind), "{kind} in {types:?}");
    }
    assert!(v.objectValue().is_none());
    assert_eq!(v.stringValue().to_string(), "");
    assert_eq!(
        (
            v.contentHuggingPriorityForOrientation(NSLayoutConstraintOrientation::Horizontal),
            v.contentCompressionResistancePriorityForOrientation(NSLayoutConstraintOrientation::Vertical),
        ),
        (250.0, 750.0)
    );
}

fn image_cells(mtm: MainThreadMarker) {
    let c = NSImageCell::new(mtm);
    assert_eq!(c.r#type(), NSCellType::NullCellType);
    assert!(c.image().is_none());
    assert_eq!(c.imageAlignment(), NSImageAlignment::AlignCenter);
    assert_eq!(c.imageScaling(), NSImageScaling::ScaleProportionallyDown);
    assert_eq!(c.imageFrameStyle(), NSImageFrameStyle::None);
    assert_eq!(c.cellSize(), size(0.0, 0.0));
    assert!(c.isEnabled() && !c.isBordered());
    // An image cell stays of the null type with an image; its object value
    // is the image.
    let image = red_image(20.0, 10.0);
    let c = NSImageCell::initImageCell(mtm.alloc::<NSImageCell>(), Some(&image));
    assert_eq!(c.r#type(), NSCellType::NullCellType);
    assert!(c.image().is_some_and(|i| std::ptr::eq(&*i, &*image)));
    let value = c.objectValue().expect("the image");
    assert!(std::ptr::eq(Retained::as_ptr(&value).cast::<NSImage>(), &*image));
    assert_eq!(c.cellSize(), size(0.0, 0.0));
    let empty = NSImageCell::initImageCell(mtm.alloc::<NSImageCell>(), None);
    assert!(empty.image().is_none());
    // Only images are its values.
    let other = NSImageCell::new(mtm);
    // SAFETY: an image is an object value.
    unsafe { other.setObjectValue(Some(&red_image(8.0, 8.0))) };
    assert!(other.image().is_some());
    // SAFETY: nil is an object value.
    unsafe { other.setObjectValue(None) };
    assert!(other.image().is_none());
    let reason = raises(|| unsafe { other.setObjectValue(Some(&NSString::from_str("hi"))) });
    assert!(reason.starts_with("NSImageCell's object value must be an NSImage"), "{reason}");
    let reason = raises(|| drop(NSImageCell::initTextCell(mtm.alloc::<NSImageCell>(), &NSString::from_str("hi"))));
    assert!(reason.starts_with("NSImageCell's object value must be an NSImage"), "{reason}");
    // Copies keep the settings.
    c.setImageAlignment(NSImageAlignment::AlignTopLeft);
    c.setImageScaling(NSImageScaling::ScaleNone);
    c.setImageFrameStyle(NSImageFrameStyle::Groove);
    // SAFETY: copy takes nothing and returns a cell of the same class.
    let copy: Retained<NSImageCell> = unsafe { msg_send![&*c, copy] };
    assert_eq!(
        (copy.imageAlignment(), copy.imageScaling(), copy.imageFrameStyle()),
        (NSImageAlignment::AlignTopLeft, NSImageScaling::ScaleNone, NSImageFrameStyle::Groove)
    );
    assert!(copy.image().is_some_and(|i| std::ptr::eq(&*i, &*image)));
}

fn image_values(mtm: MainThreadMarker) {
    let v = NSImageView::new(mtm);
    let c = v.cell().expect("a cell");
    let image = red_image(20.0, 10.0);
    v.setImage(Some(&image));
    // SAFETY: the cell is an image cell.
    let cell_image: Option<Retained<NSImage>> = unsafe { msg_send![&*c, image] };
    assert!(cell_image.is_some_and(|i| std::ptr::eq(&*i, &*image)));
    assert_eq!(c.r#type(), NSCellType::NullCellType);
    let value = v.objectValue().expect("the image");
    assert!(is_kind(&value, NSImage::class()));
    assert_eq!(v.intValue(), 0);
    assert_eq!(v.intrinsicContentSize(), size(20.0, 10.0));
    assert_eq!(fits(&v, size(100.0, 50.0)), size(20.0, 10.0));
    // The view's settings are the cell's.
    v.setImageScaling(NSImageScaling::ScaleNone);
    v.setImageAlignment(NSImageAlignment::AlignBottomRight);
    // SAFETY: the cell is an image cell.
    let cell = unsafe { &*(Retained::as_ptr(&c).cast::<NSImageCell>()) };
    assert_eq!(
        (cell.imageScaling(), cell.imageAlignment()),
        (NSImageScaling::ScaleNone, NSImageAlignment::AlignBottomRight)
    );
    // The view keeps its own target, action and tag.
    let t = target(mtm);
    // SAFETY: the target outlives the view's use of it.
    unsafe {
        v.setTarget(Some(&t));
        v.setAction(Some(sel!(changed:)));
    }
    v.setTag(5);
    assert!(v.target().is_some() && v.action() == Some(sel!(changed:)) && v.tag() == 5);
    assert_eq!((c.tag(), c.action()), (-1, None));
    v.setContinuous(true);
    assert!(v.isContinuous());
    // The intrinsic size is the image's alignment rect.
    let aligned = red_image(20.0, 10.0);
    aligned.setAlignmentRect(rect(2.0, 1.0, 16.0, 8.0));
    v.setImage(Some(&aligned));
    assert_eq!(v.intrinsicContentSize(), size(16.0, 8.0));
    assert_eq!(insets(&v), (1.0, 2.0, 1.0, 2.0));
    v.setImage(None);
    assert_eq!(v.intrinsicContentSize(), size(0.0, 0.0));
    // The factory: no frame, the image's size as its own; sent to a
    // subclass, one of that subclass.
    let w = NSImageView::imageViewWithImage(&red_image(30.0, 20.0), mtm);
    assert_eq!(w.frame(), NSRect::ZERO);
    assert_eq!(w.intrinsicContentSize(), size(30.0, 20.0));
    assert!(!w.isEditable() && w.animates());
    assert_eq!(w.imageScaling(), NSImageScaling::ScaleProportionallyDown);
    // SAFETY: the factory takes an image and returns an image view.
    let sub: Retained<NSImageView> =
        unsafe { msg_send![MyImageView::class(), imageViewWithImage: &*red_image(30.0, 20.0)] };
    assert!(is_kind(&sub, MyImageView::class()), "{:?}", sub.class().name());
    assert!(sub.image().is_some());
    // A tint and a symbol configuration are the view's.
    let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    v.setContentTintColor(Some(&red));
    assert!(v.contentTintColor().is_some());
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(26.0, 0.0);
    v.setSymbolConfiguration(Some(&config));
    assert!(v.symbolConfiguration().is_some());
}

fn frame_styles(mtm: MainThreadMarker) {
    use NSImageFrameStyle as F;
    // The cell's size, and its drawing rect in an unflipped view's bounds.
    let cases = [
        (F::None, (0.0, 0.0), rect(0.0, 0.0, 100.0, 50.0), rect(5.0, 7.0, 30.0, 30.0), rect(0.0, 0.0, 10.0, 6.0)),
        (F::Photo, (6.0, 6.0), rect(1.0, 2.0, 97.0, 47.0), rect(6.0, 9.0, 27.0, 27.0), rect(1.0, 2.0, 7.0, 3.0)),
        (
            F::GrayBezel,
            (18.0, 21.0),
            rect(8.0, 8.0, 84.0, 34.0),
            rect(13.0, 15.0, 14.0, 14.0),
            rect(5.0, 3.0, 0.0, 0.0),
        ),
        (F::Groove, (4.0, 4.0), rect(2.0, 2.0, 96.0, 46.0), rect(7.0, 9.0, 26.0, 26.0), rect(2.0, 2.0, 6.0, 2.0)),
        (F::Button, (4.0, 4.0), rect(2.0, 2.0, 96.0, 46.0), rect(7.0, 9.0, 26.0, 26.0), rect(2.0, 2.0, 6.0, 2.0)),
    ];
    let v = NSImageView::new(mtm);
    v.setImage(Some(&red_image(20.0, 10.0)));
    let c = v.cell().expect("a cell");
    for (style, (w, h), wide, square, tiny) in cases {
        v.setImageFrameStyle(style);
        let what = format!("{style:?}");
        assert_eq!(c.cellSize(), size(w, h), "{what}");
        assert_eq!(c.cellSizeForBounds(rect(0.0, 0.0, 100.0, 50.0)), size(w, h), "{what}");
        for (bounds, drawing) in [
            (rect(0.0, 0.0, 100.0, 50.0), wide),
            (rect(5.0, 7.0, 30.0, 30.0), square),
            (rect(0.0, 0.0, 10.0, 6.0), tiny),
        ] {
            assert_eq!(c.drawingRectForBounds(bounds), drawing, "{what} in {bounds:?}");
            // The image and title rects are the bounds, whatever the frame.
            assert_eq!((c.imageRectForBounds(bounds), c.titleRectForBounds(bounds)), (bounds, bounds), "{what}");
        }
        // Without a frame the view is as big as its image; with one it has
        // no size of its own, and its alignment rect is 3 points in.
        let framed = style != F::None;
        let intrinsic = if framed { size(-1.0, -1.0) } else { size(20.0, 10.0) };
        assert_eq!(v.intrinsicContentSize(), intrinsic, "{what}");
        assert_eq!(insets(&v), if framed { (3.0, 3.0, 3.0, 3.0) } else { (0.0, 0.0, 0.0, 0.0) }, "{what}");
        let fitted = if framed { size(100.0, 50.0) } else { size(20.0, 10.0) };
        assert_eq!(fits(&v, size(100.0, 50.0)), fitted, "{what}");
        // A photo, a groove and a button are opaque.
        assert_eq!(v.isOpaque(), matches!(style, F::Photo | F::Groove | F::Button), "{what}");
    }
    // A cell shown by no view measures the same.
    let lone = NSImageCell::initImageCell(mtm.alloc::<NSImageCell>(), Some(&red_image(20.0, 10.0)));
    lone.setImageFrameStyle(F::Photo);
    assert_eq!(lone.drawingRectForBounds(rect(0.0, 0.0, 100.0, 50.0)), rect(1.0, 2.0, 97.0, 47.0));
}

// Where the image goes

fn is_red(p: [u8; 4]) -> bool {
    p[0] > 180 && p[1] < 80 && p[2] < 80 && p[3] > 180
}

/// The box (points, from the view's bottom left) the red pixels of a
/// snapshot at `scale` cover.
fn red_box(rep: &NSBitmapImageRep, scale: f64) -> Option<NSRect> {
    let (w, h) = (rep.pixelsWide(), rep.pixelsHigh());
    let (mut x0, mut y0, mut x1, mut y1) = (isize::MAX, isize::MAX, -1, -1);
    for y in 0..h {
        for x in 0..w {
            if is_red(pixel(rep, x, y)) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    if x1 < 0 {
        return None;
    }
    let (width, height) = ((x1 - x0 + 1) as f64 / scale, (y1 - y0 + 1) as f64 / scale);
    let top = y0 as f64 / scale;
    Some(rect(x0 as f64 / scale, h as f64 / scale - top - height, width, height))
}

/// Alignments with where each puts an image across (0 left, 1 right) and
/// up (0 bottom, 1 top).
const ALIGNMENTS: [(NSImageAlignment, f64, f64); 9] = [
    (NSImageAlignment::AlignCenter, 0.5, 0.5),
    (NSImageAlignment::AlignTop, 0.5, 1.0),
    (NSImageAlignment::AlignTopLeft, 0.0, 1.0),
    (NSImageAlignment::AlignTopRight, 1.0, 1.0),
    (NSImageAlignment::AlignLeft, 0.0, 0.5),
    (NSImageAlignment::AlignBottom, 0.5, 0.0),
    (NSImageAlignment::AlignBottomLeft, 0.0, 0.0),
    (NSImageAlignment::AlignBottomRight, 1.0, 0.0),
    (NSImageAlignment::AlignRight, 1.0, 0.5),
];

/// Half-way values up, as AppKit rounds an image's origin.
fn half_up(v: f64) -> f64 {
    (v + 0.5).floor()
}

/// Where an image lands, as measured on macOS: scaled into the drawing
/// rect, aligned, its origin rounded to whole points (half-way up), and
/// clipped by the view.
fn landing(image: NSSize, area: NSRect, view: NSSize, scaling: NSImageScaling, across: f64, up: f64) -> NSRect {
    let fit = || {
        let k = (area.size.width / image.width).min(area.size.height / image.height);
        NSSize::new(image.width * k, image.height * k)
    };
    let s = match scaling {
        NSImageScaling::ScaleAxesIndependently => area.size,
        NSImageScaling::ScaleNone => image,
        NSImageScaling::ScaleProportionallyUpOrDown => fit(),
        _ if image.width <= area.size.width && image.height <= area.size.height => image,
        _ => fit(),
    };
    let x = half_up(area.origin.x + across * (area.size.width - s.width));
    let y = half_up(area.origin.y + up * (area.size.height - s.height));
    // Clipped by the view.
    let (x0, y0) = (x.max(0.0), y.max(0.0));
    let (x1, y1) = ((x + s.width).min(view.width), (y + s.height).min(view.height));
    rect(x0, y0, x1 - x0, y1 - y0)
}

fn image_placement(mtm: MainThreadMarker) {
    let scalings = [
        NSImageScaling::ScaleProportionallyDown,
        NSImageScaling::ScaleAxesIndependently,
        NSImageScaling::ScaleNone,
        NSImageScaling::ScaleProportionallyUpOrDown,
    ];
    // Images and views chosen so that every edge lands on a whole point:
    // smaller and larger than the view, odd sizes whose centering rounds.
    let cases = [
        ((20.0, 10.0), (100.0, 50.0)),
        ((200.0, 100.0), (100.0, 80.0)),
        ((21.0, 11.0), (100.0, 50.0)),
        ((10.0, 20.0), (100.0, 50.0)),
    ];
    for scale in [1.0, 2.0] {
        for ((iw, ih), (vw, vh)) in cases {
            let v = image_view(mtm, rect(0.0, 0.0, vw, vh));
            v.setAppearance(Some(&aqua()));
            v.setImage(Some(&red_image(iw, ih)));
            for scaling in scalings {
                // A 21 × 11 image scaled up has edges between pixels.
                if (iw, scaling) == (21.0, NSImageScaling::ScaleProportionallyUpOrDown) {
                    continue;
                }
                v.setImageScaling(scaling);
                for (alignment, across, up) in ALIGNMENTS {
                    v.setImageAlignment(alignment);
                    let rep = snapshot(&v, scale);
                    let want = landing(size(iw, ih), rect(0.0, 0.0, vw, vh), size(vw, vh), scaling, across, up);
                    assert_eq!(
                        red_box(&rep, scale),
                        Some(want),
                        "{iw}x{ih} in {vw}x{vh} {scaling:?} {alignment:?} at {scale}x"
                    );
                }
            }
        }
    }
}

fn frames_place_the_image(mtm: MainThreadMarker) {
    use NSImageFrameStyle as F;
    let cases = [
        (F::Photo, rect(1.0, 2.0, 97.0, 47.0)),
        (F::GrayBezel, rect(8.0, 8.0, 84.0, 34.0)),
        (F::Groove, rect(2.0, 2.0, 96.0, 46.0)),
        (F::Button, rect(2.0, 2.0, 96.0, 46.0)),
    ];
    for (style, area) in cases {
        let v = image_view(mtm, rect(0.0, 0.0, 100.0, 50.0));
        v.setAppearance(Some(&aqua()));
        v.setImage(Some(&red_image(20.0, 10.0)));
        v.setImageFrameStyle(style);
        for (scaling, alignments) in [
            (NSImageScaling::ScaleProportionallyDown, &ALIGNMENTS[..]),
            (NSImageScaling::ScaleAxesIndependently, &ALIGNMENTS[..1]),
        ] {
            v.setImageScaling(scaling);
            for &(alignment, across, up) in alignments {
                v.setImageAlignment(alignment);
                let rep = snapshot(&v, 2.0);
                let want = landing(size(20.0, 10.0), area, size(100.0, 50.0), scaling, across, up);
                assert_eq!(red_box(&rep, 2.0), Some(want), "{style:?} {scaling:?} {alignment:?}");
            }
        }
    }
}

fn nothing_without_an_image(mtm: MainThreadMarker) {
    let v = image_view(mtm, rect(0.0, 0.0, 20.0, 20.0));
    let rep = snapshot(&v, 1.0);
    for (x, y) in [(0, 0), (10, 10), (19, 19)] {
        assert_px(&rep, x, y, CLEAR);
    }
}

// Tints

/// The pixel `color` fills with, drawn under `appearance`.
fn reference(mtm: MainThreadMarker, appearance: &NSAppearance, color: Retained<NSColor>) -> [u8; 4] {
    let view = draw_view(mtm, rect(0.0, 0.0, 4.0, 4.0), false, move |_, _| {
        color.set();
        NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
    });
    view.setAppearance(Some(appearance));
    pixel(&snapshot(&view, 1.0), 2, 2)
}

fn template_tints(mtm: MainThreadMarker) {
    let template = filled(10.0, 10.0, [0.0, 0.0, 0.0, 1.0]);
    template.setTemplate(true);
    let plain = filled(10.0, 10.0, [0.0, 0.0, 0.0, 1.0]);
    let blue = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0);
    // SAFETY: the names are AppKit's constants.
    let appearances = unsafe { [NSAppearanceNameAqua, NSAppearanceNameDarkAqua] };
    for name in appearances {
        let appearance = NSAppearance::appearanceNamed(name).expect("an appearance");
        let shown = |image: &NSImage, setup: &dyn Fn(&NSImageView)| {
            let v = image_view(mtm, rect(0.0, 0.0, 10.0, 10.0));
            v.setAppearance(Some(&appearance));
            v.setImage(Some(image));
            setup(&v);
            pixel(&snapshot(&v, 1.0), 5, 5)
        };
        let near_enough = |a: [u8; 4], b: [u8; 4]| a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= 3);
        let what = name.to_string();
        // Untinted, a template draws in the secondary label color; disabled,
        // in the disabled text color.
        let secondary = reference(mtm, &appearance, NSColor::secondaryLabelColor());
        let got = shown(&template, &|_| {});
        assert!(near_enough(got, secondary), "{what}: {got:?}, not {secondary:?}");
        let disabled = reference(mtm, &appearance, NSColor::disabledControlTextColor());
        let got = shown(&template, &|v| v.setEnabled(false));
        assert!(near_enough(got, disabled), "{what} disabled: {got:?}, not {disabled:?}");
        // A tint is used as it is, at half strength when disabled.
        assert!(near_enough(shown(&template, &|v| v.setContentTintColor(Some(&blue))), [0, 0, 255, 255]), "{what}");
        let got = shown(&template, &|v| {
            v.setContentTintColor(Some(&blue));
            v.setEnabled(false);
        });
        assert!(near_enough(got, [0, 0, 128, 128]), "{what} tinted, disabled: {got:?}");
        // On an emphasized background (a selected row) it's white, tint or
        // not.
        for tinted in [false, true] {
            let got = shown(&template, &|v| {
                v.cell().expect("a cell").setBackgroundStyle(NSBackgroundStyle::Emphasized);
                if tinted {
                    v.setContentTintColor(Some(&blue));
                }
            });
            assert!(near_enough(got, [255, 255, 255, 255]), "{what} emphasized: {got:?}");
        }
        // Other images keep their colors, fading when disabled.
        assert!(near_enough(shown(&plain, &|v| v.setContentTintColor(Some(&blue))), [0, 0, 0, 255]), "{what}");
        let got = shown(&plain, &|v| v.setEnabled(false));
        assert!(near_enough(got, [0, 0, 0, 102]), "{what} plain, disabled: {got:?}");
        let got = shown(&plain, &|v| v.cell().expect("a cell").setBackgroundStyle(NSBackgroundStyle::Emphasized));
        assert!(near_enough(got, [0, 0, 0, 255]), "{what} plain, emphasized: {got:?}");
    }
}

// Symbol images

fn symbol_images(mtm: MainThreadMarker) {
    let symbol = |name: &str| {
        NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)
            .unwrap_or_else(|| panic!("the {name} symbol"))
    };
    let star = symbol("star.fill");
    let v = image_view(mtm, rect(0.0, 0.0, 40.0, 40.0));
    v.setImage(Some(&star));
    // As big as the symbol's alignment rect.
    assert_eq!(v.intrinsicContentSize(), star.alignmentRect().size);
    // A symbol configuration makes the symbol it shows bigger.
    let small = v.intrinsicContentSize();
    v.setSymbolConfiguration(Some(&NSImageSymbolConfiguration::configurationWithPointSize_weight(26.0, 0.0)));
    let big = v.intrinsicContentSize();
    assert!(big.width > small.width * 1.5 && big.height > small.height * 1.5, "{small:?} to {big:?}");
    v.setSymbolConfiguration(None);
    assert_eq!(v.intrinsicContentSize(), small);
    // Symbols are templates: a tint colors them.
    let circle = symbol("circle.fill");
    let v = image_view(mtm, rect(0.0, 0.0, 30.0, 30.0));
    v.setAppearance(Some(&aqua()));
    v.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    v.setImage(Some(&circle));
    v.setContentTintColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0)));
    assert_px(&snapshot(&v, 2.0), 30, 30, BLUE);
    // A button's symbol configuration grows its symbol too.
    // SAFETY: no target or action.
    let b = unsafe { NSButton::buttonWithImage_target_action(&star, None, None, mtm) };
    b.setBezelStyle(NSBezelStyle::FlexiblePush);
    let before = b.intrinsicContentSize();
    b.setSymbolConfiguration(Some(&NSImageSymbolConfiguration::configurationWithPointSize_weight(26.0, 0.0)));
    let after = b.intrinsicContentSize();
    assert!(after.width > before.width && after.height > before.height, "{before:?} to {after:?}");
}

// Editing

fn delete_key() -> Retained<NSEvent> {
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::ZERO,
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        &NSString::from_str("\u{7f}"),
        &NSString::from_str("\u{7f}"),
        false,
        51,
    )
    .expect("a key event")
}

fn editing(mtm: MainThreadMarker) {
    let t = target(mtm);
    let v = image_view(mtm, rect(0.0, 0.0, 40.0, 40.0));
    v.setImage(Some(&red_image(20.0, 20.0)));
    // SAFETY: the target outlives the view's use of it.
    unsafe {
        v.setTarget(Some(&t));
        v.setAction(Some(sel!(changed:)));
    }
    // Not editable: the keyboard and delete: leave the image.
    v.keyDown(&delete_key());
    // SAFETY: delete: takes a sender.
    let _: () = unsafe { msg_send![&*v, delete: None::<&AnyObject>] };
    assert!(v.image().is_some());
    assert!(take_actions().is_empty());
    // Editable: it takes the keyboard, and Delete clears it and sends the
    // action.
    v.setEditable(true);
    let c = v.cell().expect("a cell");
    assert!(c.isEditable() && c.isSelectable());
    assert!(v.acceptsFirstResponder());
    v.keyDown(&delete_key());
    assert!(v.image().is_none());
    assert_eq!(take_actions(), ["changed: image false"]);
    v.setImage(Some(&red_image(20.0, 20.0)));
    // SAFETY: as above.
    let _: () = unsafe { msg_send![&*v, delete: None::<&AnyObject>] };
    assert!(v.image().is_none());
    assert_eq!(take_actions(), ["changed: image false"]);
    // Setting the image isn't an edit.
    v.setImage(Some(&red_image(20.0, 20.0)));
    assert!(take_actions().is_empty());
    // Disabled, it doesn't take the keyboard.
    v.setEnabled(false);
    assert!(!v.acceptsFirstResponder());
    v.setEnabled(true);
    v.setEditable(false);
    assert!(!v.acceptsFirstResponder());
    take_actions();
    // Disabled but editable, it still deletes, by key and by delete:, and
    // Delete, Backspace and Forward Delete all do.
    v.setEditable(true);
    v.setEnabled(false);
    for key in ["\u{7f}", "\u{8}", "\u{f728}"] {
        v.setImage(Some(&red_image(20.0, 20.0)));
        v.keyDown(&key_event(key));
        assert!(v.image().is_none(), "{key:?}");
        assert_eq!(take_actions(), ["changed: image false"], "{key:?}");
    }
    v.setImage(Some(&red_image(20.0, 20.0)));
    // SAFETY: as above.
    let _: () = unsafe { msg_send![&*v, delete: None::<&AnyObject>] };
    assert!(v.image().is_none());
    assert_eq!(take_actions(), ["changed: image false"]);
    // With no image to delete, nothing happens.
    v.setEnabled(true);
    v.keyDown(&delete_key());
    // SAFETY: as above.
    let _: () = unsafe { msg_send![&*v, delete: None::<&AnyObject>] };
    assert!(take_actions().is_empty());
}

fn key_event(characters: &str) -> Retained<NSEvent> {
    let s = NSString::from_str(characters);
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::ZERO,
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        &s,
        &s,
        false,
        51,
    )
    .expect("a key event")
}

fn validates(view: &NSImageView, action: Sel) -> bool {
    let item = NSMenuItem::new(MainThreadMarker::from(view));
    // SAFETY: any selector may be an action.
    unsafe { item.setAction(Some(action)) };
    // SAFETY: validateMenuItem: takes a menu item and returns BOOL.
    unsafe { msg_send![view, validateMenuItem: &*item] }
}

fn edit_menu(mtm: MainThreadMarker) {
    // (Paste depends on what the general pasteboard holds, and cut and
    // copy write it, so they're left to the Linux tests.)
    let v = image_view(mtm, rect(0.0, 0.0, 40.0, 40.0));
    let items =
        |v: &NSImageView| [sel!(copy:), sel!(cut:), sel!(delete:), sel!(selectAll:)].map(|action| validates(v, action));
    v.setEditable(true);
    assert_eq!(items(&v), [false, false, false, true], "editable, no image");
    v.setImage(Some(&red_image(20.0, 20.0)));
    assert_eq!(items(&v), [true, true, true, true], "editable");
    v.setEnabled(false);
    assert_eq!(items(&v), [true, true, true, true], "editable, disabled");
    v.setEditable(false);
    v.setEnabled(true);
    assert_eq!(items(&v), [true, false, false, true], "not editable");
    assert!(!validates(&v, sel!(paste:)), "paste, not editable");
    // Without cut, copy and paste, deleting isn't offered or done either.
    v.setAllowsCutCopyPaste(false);
    v.setEditable(true);
    assert_eq!(items(&v), [false, false, false, true], "no cut, copy and paste");
    let t = target(mtm);
    // SAFETY: the target outlives the view's use of it.
    unsafe {
        v.setTarget(Some(&t));
        v.setAction(Some(sel!(changed:)));
    }
    take_actions();
    // SAFETY: delete: takes a sender.
    let _: () = unsafe { msg_send![&*v, delete: None::<&AnyObject>] };
    v.keyDown(&delete_key());
    assert!(v.image().is_some());
    assert!(take_actions().is_empty());
}

// Drags, with a dragging info of our own, as a drag session would hand
// one over (`draggingUpdated:` isn't called: AppKit's asks the session for
// more than the protocol has).

pub struct DragIvars {
    board: Retained<NSPasteboard>,
    mask: Cell<NSDragOperation>,
    source: RefCell<Option<Retained<AnyObject>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceImageDragInfo"]
    #[ivars = DragIvars]
    struct DragInfo;

    impl DragInfo {
        #[unsafe(method_id(draggingPasteboard))]
        fn dragging_pasteboard(&self) -> Retained<NSPasteboard> {
            self.ivars().board.clone()
        }

        #[unsafe(method(draggingSourceOperationMask))]
        fn dragging_source_operation_mask(&self) -> NSDragOperation {
            self.ivars().mask.get()
        }

        #[unsafe(method_id(draggingSource))]
        fn dragging_source(&self) -> Option<Retained<AnyObject>> {
            self.ivars().source.borrow().clone()
        }

        #[unsafe(method(draggingLocation))]
        fn dragging_location(&self) -> NSPoint {
            NSPoint::new(10.0, 10.0)
        }

        #[unsafe(method(draggedImageLocation))]
        fn dragged_image_location(&self) -> NSPoint {
            NSPoint::new(10.0, 10.0)
        }

        #[unsafe(method_id(draggingDestinationWindow))]
        fn dragging_destination_window(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(draggingSequenceNumber))]
        fn dragging_sequence_number(&self) -> isize {
            1
        }

        #[unsafe(method(numberOfValidItemsForDrop))]
        fn number_of_valid_items_for_drop(&self) -> isize {
            1
        }

        #[unsafe(method(setNumberOfValidItemsForDrop:))]
        fn set_number_of_valid_items_for_drop(&self, _n: isize) {}

        #[unsafe(method(animatesToDestination))]
        fn animates_to_destination(&self) -> bool {
            false
        }

        #[unsafe(method(setAnimatesToDestination:))]
        fn set_animates_to_destination(&self, _flag: bool) {}

        #[unsafe(method(draggingFormation))]
        fn dragging_formation(&self) -> isize {
            0
        }

        #[unsafe(method(setDraggingFormation:))]
        fn set_dragging_formation(&self, _formation: isize) {}

        #[unsafe(method(springLoadingHighlight))]
        fn spring_loading_highlight(&self) -> isize {
            0
        }

        #[unsafe(method(resetSpringLoading))]
        fn reset_spring_loading(&self) {}

        #[unsafe(method_id(draggedImage))]
        fn dragged_image(&self) -> Option<Retained<AnyObject>> {
            None
        }
    }

    unsafe impl NSObjectProtocol for DragInfo {}
);

fn drag_info(mtm: MainThreadMarker, board: &NSPasteboard, mask: NSDragOperation) -> Retained<DragInfo> {
    let ivars = DragIvars { board: board.retain(), mask: Cell::new(mask), source: RefCell::new(None) };
    // SAFETY: NSObject's initializer.
    unsafe { msg_send![super(DragInfo::alloc(mtm).set_ivars(ivars)), init] }
}

fn entered(view: &NSImageView, info: &DragInfo) -> NSDragOperation {
    // SAFETY: draggingEntered: takes dragging info and returns an operation.
    unsafe { msg_send![view, draggingEntered: info] }
}

fn drags(mtm: MainThreadMarker) {
    use NSDragOperation as Op;
    let t = target(mtm);
    let png = std::fs::read(fixture("halves.png").to_string()).expect("the PNG");
    // A pasteboard of our own, holding an image's data, or text.
    let board = NSPasteboard::pasteboardWithUniqueName();
    let holding = |image: bool| {
        board.clearContents();
        if image {
            // SAFETY: the type is a pasteboard type.
            board.setData_forType(Some(&NSData::with_bytes(&png)), &NSString::from_str("public.png"));
        } else {
            board.setString_forType(&NSString::from_str("hi"), &NSString::from_str("public.utf8-plain-text"));
        }
    };
    let view = |editable: bool, enabled: bool| {
        let v = image_view(mtm, rect(0.0, 0.0, 40.0, 40.0));
        v.setEditable(editable);
        v.setEnabled(enabled);
        // SAFETY: the target outlives the view's use of it.
        unsafe {
            v.setTarget(Some(&t));
            v.setAction(Some(sel!(changed:)));
        }
        v
    };
    // An editable view takes a drag with an image, enabled or not, if its
    // source allows a copy (or leaves the choice to it).
    holding(true);
    for (editable, enabled, mask, want) in [
        (false, true, Op::Copy, Op::None),
        (true, true, Op::Copy, Op::Copy),
        (true, false, Op::Copy, Op::Copy),
        (true, true, Op::Generic, Op::Copy),
        (true, true, Op::Link, Op::None),
        (true, true, Op::Move, Op::None),
        (true, true, Op::Link | Op::Move, Op::None),
        (true, true, Op::Copy | Op::Move, Op::Copy),
        (true, true, Op::Every, Op::Copy),
    ] {
        let v = view(editable, enabled);
        let info = drag_info(mtm, &board, mask);
        assert_eq!(entered(&v, &info), want, "editable {editable} enabled {enabled} source {mask:?}");
    }
    // Not with no image; one from the view itself is taken as any other.
    holding(false);
    assert_eq!(entered(&view(true, true), &drag_info(mtm, &board, Op::Copy)), Op::None);
    holding(true);
    let v = view(true, true);
    let info = drag_info(mtm, &board, Op::Copy);
    let source: Retained<AnyObject> = v.clone().into();
    info.ivars().source.replace(Some(source));
    assert_eq!(entered(&v, &info), Op::Copy);
    // Dropped, the drop succeeds if the source allows a copy (whatever the
    // drag holds); when the drop is over, the image becomes the view's, and
    // it sends its action.
    take_actions();
    for (mask, performed) in [(Op::Copy, true), (Op::Generic, true), (Op::Link, false)] {
        let v = view(true, false);
        let info = drag_info(mtm, &board, mask);
        entered(&v, &info);
        // SAFETY: the destination methods take dragging info; the first two
        // return BOOL.
        let (prepared, done): (bool, bool) =
            unsafe { (msg_send![&*v, prepareForDragOperation: &*info], msg_send![&*v, performDragOperation: &*info]) };
        assert!(prepared, "{mask:?}");
        assert_eq!(done, performed, "{mask:?}");
        assert!(v.image().is_none() && take_actions().is_empty(), "{mask:?}");
        if done {
            // SAFETY: as above.
            let _: () = unsafe { msg_send![&*v, concludeDragOperation: &*info] };
            assert_eq!(v.image().map(|i| i.size()), Some(size(4.0, 4.0)), "{mask:?}");
            assert_eq!(take_actions(), ["changed: image true"], "{mask:?}");
        }
    }
    // A drop of no image changes nothing.
    holding(false);
    let v = view(true, true);
    let info = drag_info(mtm, &board, Op::Copy);
    entered(&v, &info);
    // SAFETY: as above.
    let done: bool = unsafe { msg_send![&*v, performDragOperation: &*info] };
    // SAFETY: as above.
    let _: () = unsafe { msg_send![&*v, concludeDragOperation: &*info] };
    assert!(done && v.image().is_none());
    assert!(take_actions().is_empty());
    // SAFETY: releaseGlobally takes nothing; the board isn't used after.
    let _: () = unsafe { msg_send![&*board, releaseGlobally] };
}

// Clicks

fn mouse(window: &NSWindow, kind: NSEventType, x: f64, y: f64) -> Retained<NSEvent> {
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind,
        NSPoint::new(x, y),
        NSEventModifierFlags::empty(),
        0.0,
        window.windowNumber(),
        None,
        0,
        1,
        1.0,
    )
    .expect("a mouse event")
}

/// The events still queued, taken.
fn drain(app: &NSApplication) -> Vec<NSEventType> {
    let mut left = Vec::new();
    // SAFETY: the mode is a constant string.
    let mode = unsafe { objc2_foundation::NSDefaultRunLoopMode };
    while let Some(e) = app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, None, mode, true) {
        left.push(e.r#type());
    }
    left
}

fn clicks(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: a titled window, never shown.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 200.0, 100.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is released by its last reference, not on close.
    unsafe { window.setReleasedWhenClosed(false) };
    let v = image_view(mtm, rect(10.0, 10.0, 40.0, 40.0));
    v.setImage(Some(&red_image(20.0, 20.0)));
    let t = target(mtm);
    // SAFETY: the target outlives the view's use of it.
    unsafe {
        v.setTarget(Some(&t));
        v.setAction(Some(sel!(changed:)));
    }
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    window.setContentView(Some(&content));
    content.addSubview(&v);
    // An image view doesn't track the mouse: a click sends no action, and
    // leaves the rest of the click for whoever takes it.
    drain(&app);
    app.postEvent_atStart(&mouse(&window, NSEventType::LeftMouseUp, 30.0, 30.0), false);
    take_actions();
    v.mouseDown(&mouse(&window, NSEventType::LeftMouseDown, 30.0, 30.0));
    assert_eq!(drain(&app), [NSEventType::LeftMouseUp]);
    assert!(take_actions().is_empty());
    window.close();
}

// Animated images

fn fixture(name: &str) -> Retained<NSString> {
    NSString::from_str(&format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
}

fn number(rep: &NSBitmapImageRep, key: &NSString) -> Option<f64> {
    let value = rep.valueForProperty(key)?;
    // SAFETY: frame properties are numbers.
    Some(unsafe { msg_send![&*value, doubleValue] })
}

fn animated_images(_mtm: MainThreadMarker) {
    // SAFETY: the keys are AppKit's constants.
    let (count, current, duration, loops) =
        unsafe { (NSImageFrameCount, NSImageCurrentFrame, NSImageCurrentFrameDuration, NSImageLoopCount) };
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &fixture("frames.gif")).expect("the GIF");
    assert_eq!(image.representations().count(), 1);
    let rep = image.representations().objectAtIndex(0).downcast::<NSBitmapImageRep>().expect("a bitmap");
    assert_eq!(number(&rep, count), Some(3.0));
    assert_eq!(number(&rep, current), Some(0.0));
    assert_eq!(number(&rep, loops), Some(0.0));
    let first = [255, 0, 0];
    let px = |rep: &NSBitmapImageRep| {
        let mut p = [0usize; 4];
        // SAFETY: four samples to write, inside the bitmap.
        unsafe { rep.getPixel_atX_y(std::ptr::NonNull::new(p.as_mut_ptr()).expect("samples"), 1, 1) };
        [p[0], p[1], p[2]]
    };
    assert_eq!(px(&rep), first);
    // Each frame shows for its own time; setting the current frame shows
    // its pixels.
    for (frame, seconds, color) in [(1, 0.2, [0, 255, 0]), (2, 0.3, [0, 0, 255]), (0, 0.1, first)] {
        let n = NSNumber::new_isize(frame);
        // SAFETY: the key is a property name and the value a number.
        unsafe { rep.setProperty_withValue(current, Some(&n)) };
        assert_eq!(number(&rep, current), Some(frame as f64));
        let d = number(&rep, duration).expect("a duration");
        assert!((d - seconds).abs() < 1e-6, "frame {frame} shows {d}s");
        assert_eq!(px(&rep), color, "frame {frame}");
    }
    // Played once (no loop count), or twice (told to play again once).
    for (name, times) in [("once.gif", 1.0), ("twice.gif", 2.0)] {
        let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &fixture(name)).expect("the GIF");
        let rep = image.representations().objectAtIndex(0).downcast::<NSBitmapImageRep>().expect("a bitmap");
        assert_eq!(number(&rep, count), Some(3.0), "{name}");
        assert_eq!(number(&rep, loops), Some(times), "{name}");
    }
    // A GIF of one frame, or another file, has no frames to tell of.
    for name in ["small.gif", "halves.png"] {
        let one = NSImage::initWithContentsOfFile(NSImage::alloc(), &fixture(name)).expect("an image");
        let rep = one.representations().objectAtIndex(0).downcast::<NSBitmapImageRep>().expect("a bitmap");
        assert_eq!(number(&rep, count), None, "{name}");
        assert_eq!(number(&rep, current), None, "{name}");
    }
}

fn frame_of(image: &NSImage) -> usize {
    let rep = image.representations().objectAtIndex(0).downcast::<NSBitmapImageRep>().expect("a bitmap");
    // SAFETY: the key is AppKit's constant.
    number(&rep, unsafe { NSImageCurrentFrame }).expect("a frame") as usize
}

/// Run the run loop until `done` says the frames `image` has shown (each
/// new one noted as it comes) are enough, failing after 10 seconds.
fn watch(image: &NSImage, done: impl Fn(&[usize]) -> bool) -> Vec<usize> {
    let give_up = Instant::now() + Duration::from_secs(10);
    let mut seen = vec![frame_of(image)];
    while !done(&seen) {
        assert!(Instant::now() < give_up, "frames seen: {seen:?}");
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01));
        let f = frame_of(image);
        if seen.last() != Some(&f) {
            seen.push(f);
        }
    }
    seen
}

/// Run the run loop for `seconds`, checking `image` stays on its frame.
fn stays(image: &NSImage, seconds: f64, what: &str) {
    let now = frame_of(image);
    let until = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < until {
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01));
        assert_eq!(frame_of(image), now, "{what}");
    }
}

/// Times the frames went back to the first.
fn wraps(seen: &[usize]) -> usize {
    seen.windows(2).filter(|w| w[1] < w[0]).count()
}

fn animation(mtm: MainThreadMarker) {
    let load = |name: &str| NSImage::initWithContentsOfFile(NSImage::alloc(), &fixture(name)).expect("the GIF");
    // A view animates in no window at all, round and round.
    let gif = load("frames.gif");
    let v = image_view(mtm, rect(0.0, 0.0, 4.0, 4.0));
    v.setImage(Some(&gif));
    let seen = watch(&gif, |seen| wraps(seen) == 1);
    assert_eq!(seen, [0, 1, 2, 0]);
    // Told not to, it stops where it is; told to again, it goes on.
    v.setAnimates(false);
    stays(&gif, 0.7, "not animating");
    let now = frame_of(&gif);
    v.setAnimates(true);
    let seen = watch(&gif, |seen| seen.len() == 2);
    assert_eq!(seen, [now, (now + 1) % 3]);
    // Without the image, it stops.
    v.setImage(None);
    stays(&gif, 0.7, "the image taken away");
    // Nor does it animate what it doesn't animate.
    let other = load("frames.gif");
    v.setAnimates(false);
    v.setImage(Some(&other));
    stays(&other, 0.5, "never animated");
    // A GIF played a number of times stops on its last frame after them.
    for (name, times) in [("once.gif", 1), ("twice.gif", 2)] {
        let gif = load(name);
        let v = image_view(mtm, rect(0.0, 0.0, 4.0, 4.0));
        v.setImage(Some(&gif));
        let seen = watch(&gif, |seen| wraps(seen) == times - 1 && seen.last() == Some(&2));
        stays(&gif, 0.8, name);
        assert_eq!(wraps(&seen), times - 1, "{name}: {seen:?}");
    }
    // A view that goes away stops.
    let gif = load("frames.gif");
    objc2::rc::autoreleasepool(|_| {
        let v = image_view(mtm, rect(0.0, 0.0, 4.0, 4.0));
        v.setImage(Some(&gif));
        watch(&gif, |seen| seen.len() == 2);
    });
    stays(&gif, 0.7, "the view gone");
}

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let _app = NSApplication::sharedApplication(mtm);
    let tests: &[Test] = &[
        ("image_view_defaults", image_view_defaults),
        ("image_cells", image_cells),
        ("image_values", image_values),
        ("frame_styles", frame_styles),
        ("image_placement", image_placement),
        ("frames_place_the_image", frames_place_the_image),
        ("nothing_without_an_image", nothing_without_an_image),
        ("template_tints", template_tints),
        ("symbol_images", symbol_images),
        ("editing", editing),
        ("edit_menu", edit_menu),
        ("drags", drags),
        ("clicks", clicks),
        ("animated_images", animated_images),
        ("animation", animation),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
