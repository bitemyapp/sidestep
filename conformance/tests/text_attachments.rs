//! Text attachments, checked against macOS: `NSTextAttachment` and
//! `NSTextAttachmentCell` (what each holds and answers), attachments laid
//! out by string drawing, TextKit 1 and TextKit 2 (their boxes' sizes and
//! the lines they make, the glyph and hit-testing answers), drawn (the
//! pixels where their images go, upright in flipped views and not), and
//! edited in a text view (one character to move over and delete, copied
//! and pasted as RTFD).
//!
//! Sizes are checked against the same text measured without the
//! attachment and relations between them (a line's ascent and descent as
//! its font gives them), never as numbers, so they hold whatever fonts a
//! system has (DejaVu alone on CI's Ubuntu).
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

mod common;

use std::cell::RefCell;

use common::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAttachmentAttributeName, NSAttributedStringAttachmentConveniences, NSAttributedStringDocumentFormats,
    NSAttributedStringKitAdditions, NSAttributedStringNSExtendedStringDrawing, NSAttributedStringNSStringDrawing,
    NSBaselineOffsetAttributeName, NSBitmapImageRep, NSButton, NSCell, NSFont, NSFontAttributeName, NSGlyphProperty,
    NSImage, NSLayoutManager, NSPasteboard, NSStandardKeyBindingResponding, NSStringDrawingOptions, NSTextAttachment,
    NSTextAttachmentCell, NSTextAttachmentCellProtocol, NSTextAttachmentContainer, NSTextContainer,
    NSTextElementProvider, NSTextField, NSTextStorage, NSTextView, NSView,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSAttributedString, NSCopying, NSData, NSDictionary, NSFileWrapper, NSMutableAttributedString, NSNumber, NSPoint,
    NSRange, NSRect, NSSize, NSString,
};

use sidestep as _;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn font() -> Retained<NSFont> {
    NSFont::userFontOfSize(12.0).expect("the user font")
}

fn font_attrs() -> Retained<NSDictionary<NSString, AnyObject>> {
    let f = font();
    // SAFETY: a constant key.
    NSDictionary::from_slices(&[unsafe { NSFontAttributeName }], &[&*f as &AnyObject])
}

/// A `w` × `h` point image of `rgba`, one pixel a point.
fn solid(w: isize, h: isize, rgba: [u8; 4]) -> Retained<NSImage> {
    let rep = bitmap(w, h);
    fill_bitmap(&rep, rgba);
    image_of(&rep)
}

fn image_of(rep: &NSBitmapImageRep) -> Retained<NSImage> {
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(rep.pixelsWide() as f64, rep.pixelsHigh() as f64));
    image.addRepresentation(rep);
    image
}

/// An image `w` × `h` whose top half is red and bottom half blue.
fn halves(w: isize, h: isize) -> Retained<NSImage> {
    let rep = bitmap(w, h);
    fill_bitmap(&rep, BLUE);
    for y in 0..h / 2 {
        for x in 0..w {
            let at = y as usize * rep.bytesPerRow() as usize + x as usize * 4;
            // SAFETY: inside the bitmap.
            unsafe { std::ptr::write(rep.bitmapData().add(at).cast::<[u8; 4]>(), RED) };
        }
    }
    image_of(&rep)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

/// An attachment of `image` with `bounds`.
fn attachment(image: Option<&NSImage>, bounds: CGRect) -> Retained<NSTextAttachment> {
    let a = NSTextAttachment::new();
    a.setImage(image);
    a.setBounds(bounds);
    a
}

/// `before`, the attachment, `after`, all in the user font.
fn with_attachment(a: &NSTextAttachment, before: &str, after: &str) -> Retained<NSMutableAttributedString> {
    let attrs = font_attrs();
    let s = NSMutableAttributedString::new();
    // SAFETY: the dictionary holds valid attributes.
    unsafe {
        s.appendAttributedString(&NSAttributedString::new_with_attributes(&NSString::from_str(before), &attrs));
        s.appendAttributedString(&NSAttributedString::attributedStringWithAttachment(a));
        s.appendAttributedString(&NSAttributedString::new_with_attributes(&NSString::from_str(after), &attrs));
        let f = font();
        s.addAttribute_value_range(NSFontAttributeName, &f, NSRange::new(0, s.length()));
    }
    s
}

fn plain(text: &str) -> Retained<NSAttributedString> {
    // SAFETY: the dictionary holds valid attributes.
    unsafe { NSAttributedString::new_with_attributes(&NSString::from_str(text), &font_attrs()) }
}

/// A line's ascent and descent in the user font: what `boundingRect…`
/// without line fragments says of "x" (its origin is the descent below the
/// baseline).
fn line_metrics() -> (f64, f64) {
    let b = plain("x").boundingRectWithSize_options_context(NSSize::ZERO, NSStringDrawingOptions::empty(), None);
    (b.size.height + b.origin.y, -b.origin.y)
}

#[track_caller]
fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 0.01, "{a} is not {b}");
}

fn bounds_of(a: &NSTextAttachment) -> CGRect {
    a.attachmentBoundsForTextContainer_proposedLineFragment_glyphPosition_characterIndex(
        None,
        rect(0.0, 0.0, 100.0, 14.0),
        CGPoint::new(5.0, 11.0),
        0,
    )
}

/// What attachments hold and answer, and the attributed strings made of
/// them.
fn attachments(mtm: MainThreadMarker) {
    let a = NSTextAttachment::new();
    assert!(a.image().is_none() && a.contents().is_none() && a.fileType().is_none() && a.fileWrapper().is_none());
    assert_eq!(a.bounds(), CGRect::ZERO);
    assert_eq!(a.lineLayoutPadding(), 0.0);
    assert!(a.allowsTextAttachmentView());
    // An attachment of nothing has a cell of its own, of no size, which
    // knows it; the same each time.
    let cell = a.attachmentCell().expect("a cell");
    assert_eq!(cell.cellSize(), NSSize::ZERO);
    assert_eq!(cell.cellBaselineOffset(), NSPoint::ZERO);
    assert!(unsafe { cell.attachment() }.is_some_and(|c| std::ptr::eq(&*c, &*a)));
    assert!(std::ptr::eq(&*a.attachmentCell().expect("a cell"), &*cell));
    // Its bounds are a file icon's.
    assert_eq!(bounds_of(&a), rect(0.0, 0.0, 32.0, 32.0));
    // With an image: no cell; the image's size, unless bounds are set,
    // which are given as they are (all zero is none).
    let red = solid(20, 30, RED);
    a.setImage(Some(&red));
    assert!(a.attachmentCell().is_none());
    assert_eq!(bounds_of(&a), rect(0.0, 0.0, 20.0, 30.0));
    let image = a.imageForBounds_textContainer_characterIndex(rect(0.0, 0.0, 20.0, 30.0), None, 0);
    assert!(image.is_some_and(|i| std::ptr::eq(&*i, &*red)));
    a.setBounds(rect(0.0, -5.0, 40.0, 10.0));
    assert_eq!(bounds_of(&a), rect(0.0, -5.0, 40.0, 10.0));
    a.setBounds(rect(0.0, -5.0, 0.0, 0.0));
    assert_eq!(bounds_of(&a), rect(0.0, -5.0, 0.0, 0.0));
    // It makes a file wrapper of its image when asked.
    assert!(a.fileWrapper().is_some_and(|w| w.isRegularFile()));
    // A cell set on an attachment with an image isn't its cell.
    let blue = NSTextAttachmentCell::initImageCell(NSTextAttachmentCell::alloc(mtm), Some(&solid(8, 9, BLUE)));
    a.setAttachmentCell(Some(ProtocolObject::from_ref(&*blue)));
    assert!(a.attachmentCell().is_none());
    // Without one, it is, and knows its attachment.
    let b = NSTextAttachment::new();
    b.setAttachmentCell(Some(ProtocolObject::from_ref(&*blue)));
    assert!(b.attachmentCell().is_some());
    assert!(unsafe { blue.attachment() }.is_some_and(|c| std::ptr::eq(&*c, &*b)));

    // Contents: kept, typed; the image decoded from them sizes the bounds;
    // a wrapper named for the type.
    let png = std::fs::read(format!("{FIXTURES}/rgba-72dpi.png")).expect("the fixture");
    let data = NSData::with_bytes(&png);
    let d = NSTextAttachment::initWithData_ofType(
        NSTextAttachment::alloc(),
        Some(&data),
        Some(&NSString::from_str("public.png")),
    );
    assert_eq!(d.contents().map(|c| c.length()), Some(png.len()));
    assert_eq!(d.fileType().map(|t| t.to_string()).as_deref(), Some("public.png"));
    assert!(d.image().is_none() && d.attachmentCell().is_none());
    assert_eq!(bounds_of(&d), rect(0.0, 0.0, 4.0, 2.0));
    assert!(
        d.imageForBounds_textContainer_characterIndex(bounds_of(&d), None, 0)
            .is_some_and(|i| i.size() == NSSize::new(4.0, 2.0))
    );
    let w = d.fileWrapper().expect("a wrapper");
    assert_eq!(w.preferredFilename().map(|n| n.to_string()).as_deref(), Some("Attachment.png"));
    assert_eq!(w.regularFileContents().map(|c| c.length()), Some(png.len()));
    let text = NSTextAttachment::initWithData_ofType(
        NSTextAttachment::alloc(),
        Some(&NSData::with_bytes(b"hello")),
        Some(&NSString::from_str("public.plain-text")),
    );
    assert_eq!(bounds_of(&text), rect(0.0, 0.0, 32.0, 32.0));
    assert_eq!(
        text.fileWrapper().and_then(|w| w.preferredFilename()).map(|n| n.to_string()).as_deref(),
        Some("Attachment.txt")
    );
    // A file wrapper: its type from its name; a cell of its image.
    let wrapper = NSFileWrapper::initRegularFileWithContents(NSFileWrapper::alloc(), &data);
    wrapper.setPreferredFilename(Some(&NSString::from_str("pic.png")));
    let f = NSTextAttachment::initWithFileWrapper(NSTextAttachment::alloc(), Some(&wrapper));
    assert!(f.contents().is_none() && f.image().is_none());
    assert_eq!(f.fileType().map(|t| t.to_string()).as_deref(), Some("public.png"));
    assert_eq!(f.attachmentCell().map(|c| c.cellSize()), Some(NSSize::new(4.0, 2.0)));
    assert_eq!(bounds_of(&f), rect(0.0, 0.0, 4.0, 2.0));

    // One character, U+FFFC, with the attachment its only attribute.
    let s = NSAttributedString::attributedStringWithAttachment(&a);
    assert_eq!(s.string().to_string(), "\u{FFFC}");
    let attrs = unsafe { s.attributesAtIndex_effectiveRange(0, std::ptr::null_mut()) };
    assert_eq!(attrs.count(), 1);
    let value = attrs.objectForKey(unsafe { NSAttachmentAttributeName }).expect("the attachment");
    assert!(std::ptr::eq(&*value as *const AnyObject as *const NSTextAttachment, &*a));
    assert!(s.containsAttachmentsInRange(NSRange::new(0, 1)));
}

/// Attachment cells: sizes, offsets and frames.
fn cells(mtm: MainThreadMarker) {
    let c = NSTextAttachmentCell::new(mtm);
    assert_eq!((c.cellSize(), c.cellBaselineOffset()), (NSSize::ZERO, NSPoint::ZERO));
    assert!(c.wantsToTrackMouse(mtm));
    assert!(unsafe { c.attachment() }.is_none());
    let c = NSTextAttachmentCell::initImageCell(NSTextAttachmentCell::alloc(mtm), Some(&solid(20, 30, RED)));
    assert_eq!(c.cellSize(), NSSize::new(20.0, 30.0));
    let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(100.0, 100.0));
    let frame = c.cellFrameForTextContainer_proposedLineFragment_glyphPosition_characterIndex(
        &tc,
        rect(0.0, 0.0, 100.0, 14.0),
        NSPoint::new(5.0, 11.0),
        0,
    );
    assert_eq!(frame, rect(0.0, 0.0, 20.0, 30.0));
}

/// Attachments measured by string drawing: as wide as their bounds (or
/// image), raising the line's ascent to their top and its descent to their
/// bottom.
fn measuring(mtm: MainThreadMarker) {
    let (ascent, descent) = line_metrics();
    let ab = plain("ab").size();
    let red = solid(20, 30, RED);
    let cases = [
        (rect(0.0, 0.0, 0.0, 0.0), 20.0, 30.0, 0.0),
        (rect(0.0, 0.0, 20.0, 30.0), 20.0, 30.0, 0.0),
        (rect(0.0, -5.0, 20.0, 30.0), 20.0, 30.0, -5.0),
        (rect(0.0, -20.0, 20.0, 30.0), 20.0, 30.0, -20.0),
        (rect(3.0, 4.0, 10.0, 5.0), 10.0, 5.0, 4.0),
        (rect(0.0, -2.5, 10.5, 5.25), 10.5, 5.25, -2.5),
        (rect(0.0, -7.5, 10.0, 30.25), 10.0, 30.25, -7.5),
        (rect(0.0, 0.0, 10.0, 0.0), 10.0, 0.0, 0.0),
    ];
    for (bounds, w, h, y) in cases {
        let s = with_attachment(&attachment(Some(&red), bounds), "a", "b");
        let size = s.size();
        close(size.width, ab.width + w);
        let (up, down) = (ascent.max(h + y), descent.max(-y));
        close(size.height, up + down);
        // One line on a baseline: the rect starts at the descent below it.
        let b = s.boundingRectWithSize_options_context(NSSize::ZERO, NSStringDrawingOptions::empty(), None);
        close(b.origin.y, -down);
        let lines =
            s.boundingRectWithSize_options_context(NSSize::ZERO, NSStringDrawingOptions::UsesLineFragmentOrigin, None);
        close(lines.size.height, up + down);
    }
    // An attachment of nothing, and one with a cell of no size: a point
    // wide; a cell's size.
    close(with_attachment(&NSTextAttachment::new(), "a", "b").size().width, ab.width + 1.0);
    let cell = |w: isize, h: isize| {
        let a = NSTextAttachment::new();
        let c = NSTextAttachmentCell::initImageCell(NSTextAttachmentCell::alloc(mtm), Some(&solid(w, h, BLUE)));
        a.setAttachmentCell(Some(ProtocolObject::from_ref(&*c)));
        with_attachment(&a, "a", "b").size()
    };
    let s = cell(5, 30);
    close(s.width, ab.width + 5.0);
    close(s.height, ascent.max(30.0) + descent);
    // Alone: its box and the font's descent.
    let alone = NSAttributedString::attributedStringWithAttachment(&attachment(Some(&red), CGRect::ZERO)).size();
    close(alone.width, 20.0);
    // On the second line.
    let s = with_attachment(&attachment(Some(&red), CGRect::ZERO), "a\n", "b");
    let r = s.boundingRectWithSize_options_context(NSSize::ZERO, NSStringDrawingOptions::UsesLineFragmentOrigin, None);
    close(r.size.height, ascent + descent + ascent.max(30.0) + descent);
    // Wrapping: an attachment starts a line when it doesn't fit, and one
    // wider than the width takes a line of its own.
    let wide = attachment(Some(&solid(50, 10, RED)), CGRect::ZERO);
    let lines = NSStringDrawingOptions::UsesLineFragmentOrigin;
    let line = ascent + descent;
    let s = with_attachment(&wide, "aaa ", " bbb");
    let narrow = plain("aaa ").size().width.max(plain(" bbb").size().width) + 1.0;
    let r = s.boundingRectWithSize_options_context(NSSize::new(narrow, 0.0), lines, None);
    close(r.size.height, 3.0 * line);
    close(r.size.width, narrow);
    let s = with_attachment(&wide, "aaa", "");
    let width = plain("aaa").size().width + 40.0;
    let r = s.boundingRectWithSize_options_context(NSSize::new(width, 0.0), lines, None);
    close(r.size.height, 2.0 * line);
    close(r.size.width, 50.0);
    let s = with_attachment(&wide, "", "bbb");
    let r = s.boundingRectWithSize_options_context(NSSize::new(plain("bbb").size().width + 40.0, 0.0), lines, None);
    close(r.size.height, 2.0 * line);
}

/// The top and bottom rows (exclusive) of the pixels in column `x` whose
/// color is near `rgba`, in a bitmap `rep`.
fn rows(rep: &NSBitmapImageRep, x: isize, rgba: [u8; 4]) -> Option<(isize, isize)> {
    let hits: Vec<isize> = (0..rep.pixelsHigh()).filter(|&y| near(pixel(rep, x, y), rgba)).collect();
    Some((*hits.first()?, *hits.last()? + 1))
}

/// Attachments drawn into their boxes, upright in flipped views and not.
fn drawing(mtm: MainThreadMarker) {
    let (_, descent) = line_metrics();
    let wa = plain("a").size().width;
    let a = attachment(Some(&solid(20, 30, RED)), rect(0.0, -5.0, 20.0, 30.0));
    let s = with_attachment(&a, "a", "b");
    // Not flipped: drawn at (10, 10), its line's bottom; the baseline is
    // the line's descent above it, the attachment 5 below that to 25
    // above.
    let rep = bitmap(100, 60);
    draw_in(&rep, |_| s.drawAtPoint(NSPoint::new(10.0, 10.0)));
    let x = (10.0 + wa + 10.0) as isize;
    let baseline = 60.0 - (10.0 + descent.max(5.0));
    assert_eq!(rows(&rep, x, RED), Some(((baseline - 25.0) as isize, (baseline + 5.0) as isize)));
    // Its left and right: from after the "a" for 20 points.
    assert!(
        near(pixel(&rep, (10.0 + wa + 1.5) as isize, 35), RED)
            && near(pixel(&rep, (10.0 + wa + 18.5) as isize, 35), RED)
    );
    assert!(!near(pixel(&rep, (10.0 + wa + 21.5) as isize, 35), RED));
    // Flipped: the text's top at 10.
    let s2 = s.clone();
    let view = draw_view(mtm, NSRect::new(NSPoint::ZERO, NSSize::new(100.0, 60.0)), true, move |_, _| {
        s2.drawAtPoint(NSPoint::new(10.0, 10.0));
    });
    let rep = snapshot(&view, 1.0);
    assert_eq!(rows(&rep, x, RED), Some((10, 40)));
    // Upright both ways: red on top, blue below.
    let two = attachment(Some(&halves(10, 20)), CGRect::ZERO);
    let s = with_attachment(&two, "", "");
    let rep = bitmap(40, 40);
    draw_in(&rep, |_| s.drawAtPoint(NSPoint::new(5.0, 5.0)));
    let (red, blue) = (rows(&rep, 9, RED).expect("red"), rows(&rep, 9, BLUE).expect("blue"));
    assert!(red.1 <= blue.0, "red {red:?} above blue {blue:?}");
    let s2 = s.clone();
    let view = draw_view(mtm, NSRect::new(NSPoint::ZERO, NSSize::new(40.0, 40.0)), true, move |_, _| {
        s2.drawAtPoint(NSPoint::new(5.0, 5.0));
    });
    let rep = snapshot(&view, 1.0);
    assert_eq!((rows(&rep, 9, RED), rows(&rep, 9, BLUE)), (Some((5, 15)), Some((15, 25))));
    // An image in bounds of another size is drawn to fill them.
    let a = attachment(Some(&solid(20, 30, RED)), rect(0.0, 0.0, 10.0, 5.0));
    let s = with_attachment(&a, "a", "b");
    let rep = bitmap(100, 60);
    draw_in(&rep, |_| s.drawAtPoint(NSPoint::new(10.0, 10.0)));
    let baseline = 60.0 - (10.0 + descent);
    let x = (10.0 + wa + 5.0) as isize;
    assert_eq!(rows(&rep, x, RED), Some(((baseline - 5.0) as isize, baseline as isize)));
    assert!(!near(pixel(&rep, (10.0 + wa + 11.5) as isize, (baseline - 2.0) as isize), RED));
}

/// A cell that records the frames it draws in.
struct Frames(RefCell<Vec<NSRect>>);

define_class!(
    #[unsafe(super(NSTextAttachmentCell, NSCell, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceRecordingAttachmentCell"]
    #[ivars = Frames]
    struct RecordingCell;

    impl RecordingCell {
        #[unsafe(method(drawWithFrame:inView:))]
        fn draw_with_frame(&self, frame: NSRect, _view: Option<&NSView>) {
            self.ivars().0.borrow_mut().push(frame);
        }

        #[unsafe(method(cellSize))]
        fn cell_size(&self) -> NSSize {
            NSSize::new(12.0, 7.0)
        }

        #[unsafe(method(cellBaselineOffset))]
        fn cell_baseline_offset(&self) -> NSPoint {
            NSPoint::new(0.0, -2.0)
        }
    }

    unsafe impl NSObjectProtocol for RecordingCell {}
);

fn recording_cell(mtm: MainThreadMarker) -> Retained<RecordingCell> {
    let this = RecordingCell::alloc(mtm).set_ivars(Frames(RefCell::new(Vec::new())));
    // SAFETY: the cell's initializer.
    unsafe { msg_send![super(this), init] }
}

/// A cell of a program's lays its attachment out by its size and baseline
/// offset, and draws it in the frame they give.
fn program_cells(mtm: MainThreadMarker) {
    let (ascent, descent) = line_metrics();
    let cell = recording_cell(mtm);
    let a = NSTextAttachment::new();
    a.setAttachmentCell(Some(ProtocolObject::from_ref(&**cell)));
    let s = with_attachment(&a, "a", "b");
    let size = s.size();
    close(size.width, plain("ab").size().width + 12.0);
    close(size.height, ascent.max(5.0) + descent.max(2.0));
    let s2 = s.clone();
    let view = draw_view(mtm, NSRect::new(NSPoint::ZERO, NSSize::new(100.0, 60.0)), true, move |_, _| {
        s2.drawAtPoint(NSPoint::new(10.0, 10.0));
    });
    let _ = snapshot(&view, 1.0);
    let frames = cell.ivars().0.borrow().clone();
    assert_eq!(frames.len(), 1, "{frames:?}");
    let f = frames[0];
    let baseline = 10.0 + ascent.max(5.0);
    close(f.origin.x, 10.0 + plain("a").size().width);
    close(f.size.width, 12.0);
    close(f.size.height, 7.0);
    // In a flipped view, the frame's top is the box's top.
    close(f.origin.y, baseline - 5.0);
}

/// A TextKit 1 layout of "a", an attachment and "b", then a second line.
fn textkit1_manager(
    a: &NSTextAttachment,
) -> (Retained<NSTextStorage>, Retained<NSLayoutManager>, Retained<NSTextContainer>) {
    let s = with_attachment(a, "a", "b\nline two");
    let storage = NSTextStorage::new();
    storage.setAttributedString(&s);
    let lm = NSLayoutManager::new();
    let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(200.0, 1000.0));
    lm.addTextContainer(&tc);
    storage.addLayoutManager(&lm);
    lm.ensureLayoutForTextContainer(&tc);
    (storage, lm, tc)
}

/// TextKit 1: the attachment's glyph, its box, the line it makes, and the
/// character under a point in it.
fn textkit1(mtm: MainThreadMarker) {
    // SAFETY: AppKit's text classes, used as they are documented.
    unsafe {
        let (_, descent) = line_metrics();
        let a = attachment(Some(&solid(20, 30, RED)), rect(0.0, -5.0, 20.0, 30.0));
        let (_storage, lm, tc) = textkit1_manager(&a);
        let wa = plain("a").size().width;
        let pad = tc.lineFragmentPadding();
        assert_eq!(lm.attachmentSizeForGlyphAtIndex(1), NSSize::new(20.0, 30.0));
        assert_eq!(lm.attachmentSizeForGlyphAtIndex(0), NSSize::new(-1.0, -1.0));
        assert_eq!(lm.propertyForGlyphAtIndex(1), NSGlyphProperty::ControlCharacter);
        let frag = lm.lineFragmentRectForGlyphAtIndex_effectiveRange(1, std::ptr::null_mut());
        let height = 25.0f64.max(line_metrics().0) + descent.max(5.0);
        close(frag.size.height, height);
        let baseline = height - descent.max(5.0);
        // The glyph's location is the box's bottom.
        let loc = lm.locationForGlyphAtIndex(1);
        close(loc.x, pad + wa);
        close(loc.y, baseline + 5.0);
        close(lm.locationForGlyphAtIndex(0).y, baseline);
        let b = lm.boundingRectForGlyphRange_inTextContainer(NSRange::new(1, 1), &tc);
        close(b.origin.x, pad + wa);
        close(b.size.width, 20.0);
        // The character under a point in the box, whichever half.
        for dx in [2.0, 10.0, 18.0] {
            let index = lm.characterIndexForPoint_inTextContainer_fractionOfDistanceBetweenInsertionPoints(
                NSPoint::new(pad + wa + dx, 10.0),
                &tc,
                std::ptr::null_mut(),
            );
            assert_eq!(index, 1, "at {dx}");
        }
        // Drawn by a text view in TextKit 1.
        let tv =
            NSTextView::initWithFrame(NSTextView::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(120.0, 80.0)));
        tv.textStorage().expect("storage").setAttributedString(&with_attachment(&a, "a", "b\nline two"));
        let _ = tv.layoutManager();
        let rep = snapshot(&tv, 1.0);
        let inset = tv.textContainerInset();
        let x = (inset.width + pad + wa + 10.0) as isize;
        let top = inset.height;
        assert_eq!(rows(&rep, x, RED), Some(((top + baseline - 25.0) as isize, (top + baseline + 5.0) as isize)));
    }
}

/// TextKit 2: the fragment's height and the attachment's frame in it.
fn textkit2(mtm: MainThreadMarker) {
    // SAFETY: AppKit's text classes, used as they are documented.
    unsafe {
        let (_, descent) = line_metrics();
        let a = attachment(Some(&solid(20, 30, RED)), rect(0.0, -5.0, 20.0, 30.0));
        let tv =
            NSTextView::initWithFrame(NSTextView::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(120.0, 80.0)));
        tv.textStorage().expect("storage").setAttributedString(&with_attachment(&a, "a", "b\nline two"));
        let tlm = tv.textLayoutManager().expect("TextKit 2");
        let tcm = tlm.textContentManager().expect("content");
        let doc = tcm.documentRange();
        tlm.ensureLayoutForRange(&doc);
        let start = doc.location();
        let fragment = tlm.textLayoutFragmentForLocation(&start).expect("a fragment");
        let height = 25.0f64.max(line_metrics().0) + descent.max(5.0);
        close(fragment.layoutFragmentFrame().size.height, height);
        assert_eq!(fragment.textAttachmentViewProviders().count(), 0);
        let at = tcm.locationFromLocation_withOffset(&start, 1).expect("a location");
        let f = fragment.frameForTextAttachmentAtLocation(&at);
        close(f.origin.x, plain("a").size().width);
        close(f.origin.y, 0.0f64.max(line_metrics().0 - 25.0));
        assert_eq!(f.size, NSSize::new(20.0, 30.0));
        // Drawn where its frame is.
        let rep = snapshot(&tv, 1.0);
        let origin = fragment.layoutFragmentFrame().origin;
        let inset = tv.textContainerInset();
        let x = (inset.width + origin.x + f.origin.x + 10.0) as isize;
        let top = inset.height + origin.y + f.origin.y;
        assert_eq!(rows(&rep, x, RED), Some((top as isize, (top + 30.0) as isize)));
    }
}

/// In a text view, an attachment is one character: moved over, selected
/// and deleted as one; copied as RTFD and pasted back.
fn editing(mtm: MainThreadMarker) {
    // SAFETY: AppKit's text classes, used as they are documented.
    unsafe {
        for tk1 in [true, false] {
            let a = attachment(Some(&solid(20, 30, RED)), CGRect::ZERO);
            let tv =
                NSTextView::initWithFrame(NSTextView::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(200.0, 80.0)));
            tv.textStorage().expect("storage").setAttributedString(&with_attachment(&a, "ab", "cd"));
            if tk1 {
                let _ = tv.layoutManager();
            }
            tv.setSelectedRange(NSRange::new(2, 0));
            tv.moveRight(None);
            assert_eq!(tv.selectedRange(), NSRange::new(3, 0), "TextKit 1: {tk1}");
            tv.moveLeft(None);
            assert_eq!(tv.selectedRange(), NSRange::new(2, 0));
            // Selected alone, it's rich text with an attachment: RTFD first.
            tv.setSelectedRange(NSRange::new(2, 1));
            let types: Vec<String> = tv.writablePasteboardTypes().iter().map(|t| t.to_string()).collect();
            assert!(types.first().is_some_and(|t| t.contains("RTFD")), "{types:?}");
            let pb = NSPasteboard::pasteboardWithUniqueName();
            assert!(tv.writeSelectionToPasteboard_types(&pb, &tv.writablePasteboardTypes()));
            // Deleted as one.
            tv.setSelectedRange(NSRange::new(2, 0));
            tv.deleteForward(None);
            assert_eq!(tv.string().to_string(), "abcd");
            // Pasted back into a view that takes graphics: an attachment
            // again, of an image its size. (One that doesn't reads the RTF,
            // which leaves attachments out.)
            tv.setImportsGraphics(true);
            tv.setSelectedRange(NSRange::new(2, 0));
            assert!(tv.readSelectionFromPasteboard(&pb));
            let storage = tv.textStorage().expect("storage");
            assert_eq!(storage.string().to_string(), "ab\u{FFFC}cd");
            let value = storage.attribute_atIndex_effectiveRange(NSAttachmentAttributeName, 2, std::ptr::null_mut());
            let pasted = value.and_then(|v| v.downcast::<NSTextAttachment>().ok()).expect("an attachment");
            let wrapper = pasted.fileWrapper().expect("its file");
            assert!(wrapper.regularFileContents().is_some_and(|c| c.length() > 0));
            let size = NSAttributedString::attributedStringWithAttachment(&pasted).size();
            close(size.width, 20.0);
        }
    }
}

/// An RTFD package (a directory of `TXT.rtf` and the files it names) read
/// from its URL: each `\NeXTGraphic` an attachment of its file.
fn packages(_: MainThreadMarker) {
    let dir = std::env::temp_dir().join(format!("sidestep-attachments-{}.rtfd", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let png = std::fs::read(format!("{FIXTURES}/wide.png")).unwrap();
    std::fs::write(dir.join("pic.png"), &png).unwrap();
    let rtf = "{\\rtf1\\ansi\\ansicpg1252\\cocoartf2822\n{\\fonttbl\\f0\\fswiss\\fcharset0 Helvetica;}\n\\pard\\f0\\fs24 \
               a{{\\NeXTGraphic pic.png \\width400 \\height600 \\appleattachmentpadding0 \\appleembedtype0 \\appleaqc\n}\\'ac}b}";
    std::fs::write(dir.join("TXT.rtf"), rtf).unwrap();
    let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(dir.to_str().unwrap()));
    // SAFETY: a file URL, no options.
    let read = unsafe {
        NSAttributedString::initWithURL_options_documentAttributes_error(
            NSAttributedString::alloc(),
            &url,
            &NSDictionary::new(),
            None,
        )
    }
    .expect("the package");
    assert_eq!(read.string().to_string(), "a\u{FFFC}b");
    // SAFETY: the key is AppKit's constant; an index in the string.
    let value = unsafe { read.attribute_atIndex_effectiveRange(NSAttachmentAttributeName, 1, std::ptr::null_mut()) };
    let a = value.and_then(|v| v.downcast::<NSTextAttachment>().ok()).expect("an attachment");
    let wrapper = a.fileWrapper().expect("its file");
    assert_eq!(wrapper.preferredFilename().map(|n| n.to_string()).as_deref(), Some("pic.png"));
    assert_eq!(wrapper.regularFileContents().map(|c| c.to_vec()), Some(png));
    let _ = std::fs::remove_dir_all(&dir);
}

/// "a", an attachment of a red image 20 × `h` (with bounds from `y`
/// below the baseline, or none), and `after`, the attachment's character
/// raised by `offset` (lowered when negative).
fn offset_attachment(h: isize, y: f64, offset: f64, after: &str) -> Retained<NSMutableAttributedString> {
    let bounds = if y == 0.0 { CGRect::ZERO } else { rect(0.0, y, 20.0, h as f64) };
    let s = with_attachment(&attachment(Some(&solid(20, h, RED)), bounds), "a", after);
    // SAFETY: a number, the attribute's value, over the attachment.
    unsafe {
        s.addAttribute_value_range(NSBaselineOffsetAttributeName, &NSNumber::new_f64(offset), NSRange::new(1, 1))
    };
    s
}

/// A baseline offset on an attachment's character raises or lowers the
/// attachment where it's drawn. String drawing and TextKit 2 make room for
/// the character's font raised or lowered and for the box as it is (which
/// may overhang the line); TextKit 1 for the box as it's drawn, as a glyph,
/// and for the character's font as it is.
fn baseline_offsets(mtm: MainThreadMarker) {
    let (ascent, descent) = line_metrics();
    let wa = plain("a").size().width;
    let cases = [(10, 0.0, 6.0), (10, 0.0, -6.0), (30, 0.0, 6.0), (30, 0.0, -6.0), (30, -5.0, 6.0), (30, -5.0, -6.0)];
    for (h, y, offset) in cases.into_iter().chain([(10, 0.0, 20.0), (10, 0.0, -20.0)]) {
        let hf = h as f64;
        let s = offset_attachment(h, y, offset, "b");
        let (up, down) = ((ascent + offset.max(0.0)).max(hf + y), (descent - offset.min(0.0)).max(-y));
        close(s.size().height, up + down);
        let rep = bitmap(100, 120);
        draw_in(&rep, |_| s.drawAtPoint(NSPoint::new(10.0, 10.0)));
        let bottom = 110.0 - down - (y + offset);
        let want = Some(((bottom - hf) as isize, bottom as isize));
        assert_eq!(rows(&rep, (10.0 + wa + 10.0) as isize, RED), want, "{h} from {y}, offset {offset}");
    }
    // SAFETY: AppKit's text classes, used as they are documented.
    unsafe {
        for (h, offset) in [(10, 6.0), (30, 6.0), (30, -6.0), (10, -20.0)] {
            let hf = h as f64;
            let s = offset_attachment(h, 0.0, offset, "b\nline two");
            // TextKit 1.
            let storage = NSTextStorage::new();
            storage.setAttributedString(&s);
            let lm = NSLayoutManager::new();
            let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(200.0, 1000.0));
            lm.addTextContainer(&tc);
            storage.addLayoutManager(&lm);
            lm.ensureLayoutForTextContainer(&tc);
            let (up, down) = (ascent.max(hf + offset.max(0.0)), descent.max(-offset.min(0.0)));
            let frag = lm.lineFragmentRectForGlyphAtIndex_effectiveRange(1, std::ptr::null_mut());
            close(frag.size.height, up + down);
            close(lm.locationForGlyphAtIndex(0).y, up);
            close(lm.locationForGlyphAtIndex(1).y, up - offset);
            let tv =
                NSTextView::initWithFrame(NSTextView::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(200.0, 80.0)));
            tv.textStorage().expect("storage").setAttributedString(&s);
            let _ = tv.layoutManager();
            let rep = snapshot(&tv, 1.0);
            let inset = tv.textContainerInset();
            let x = (inset.width + tc.lineFragmentPadding() + wa + 10.0) as isize;
            let top = inset.height + up - offset - hf;
            assert_eq!(rows(&rep, x, RED), Some((top as isize, (top + hf) as isize)), "TextKit 1: {h}, {offset}");
            // TextKit 2.
            let tv =
                NSTextView::initWithFrame(NSTextView::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(200.0, 80.0)));
            tv.textStorage().expect("storage").setAttributedString(&s);
            let tlm = tv.textLayoutManager().expect("TextKit 2");
            let tcm = tlm.textContentManager().expect("content");
            let doc = tcm.documentRange();
            tlm.ensureLayoutForRange(&doc);
            let fragment = tlm.textLayoutFragmentForLocation(&doc.location()).expect("a fragment");
            let (up, down) = ((ascent + offset.max(0.0)).max(hf), descent - offset.min(0.0));
            close(fragment.layoutFragmentFrame().size.height, up + down);
            let at = tcm.locationFromLocation_withOffset(&doc.location(), 1).expect("a location");
            let f = fragment.frameForTextAttachmentAtLocation(&at);
            close(f.origin.y, up - offset - hf);
            assert_eq!(f.size, NSSize::new(20.0, hf));
        }
    }
}

/// The rows of `rep` with red in them, and the columns of the red in the
/// middle one of them.
fn red_area(rep: &NSBitmapImageRep) -> (usize, isize) {
    let columns = |y: isize| (0..rep.pixelsWide()).filter(|&x| near(pixel(rep, x, y), RED)).count() as isize;
    let rows: Vec<isize> = (0..rep.pixelsHigh()).filter(|&y| columns(y) > 0).collect();
    let middle = rows.get(rows.len() / 2).map_or(0, |&y| columns(y));
    (rows.len(), middle)
}

/// Controls draw the attachments of their attributed strings: a label's,
/// a button's title's.
fn controls(mtm: MainThreadMarker) {
    let a = attachment(Some(&solid(20, 14, RED)), CGRect::ZERO);
    let label = NSTextField::labelWithAttributedString(&with_attachment(&a, "a", "b"), mtm);
    label.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(80.0, 30.0)));
    let (rows, columns) = red_area(&snapshot(&label, 1.0));
    assert!(rows >= 10 && (17..=21).contains(&columns), "a label: {rows} rows, {columns} columns");
    // SAFETY: a button with no target or action.
    let button = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("x"), None, None, mtm) };
    button.setAttributedTitle(&with_attachment(&a, "a", "b"));
    button.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(80.0, 30.0)));
    let (rows, columns) = red_area(&snapshot(&button, 1.0));
    assert!(rows >= 8 && (17..=21).contains(&columns), "a button: {rows} rows, {columns} columns");
}

/// What a subclass's method was sent: whether a container, the proposed
/// line fragment and glyph position, and the character's index.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Call {
    Bounds { container: bool, fragment: CGRect, position: CGPoint, index: usize },
    Bounds2 { container: bool, fragment: CGRect, position: CGPoint },
    Image { container: bool, index: usize },
    Image2 { container: bool },
}

struct Calls(RefCell<Vec<Call>>);

define_class!(
    #[unsafe(super(NSTextAttachment, objc2::runtime::NSObject))]
    #[name = "ConformanceAttachmentOfTextKit1"]
    #[ivars = Calls]
    struct OfTextKit1;

    impl OfTextKit1 {
        #[unsafe(method(attachmentBoundsForTextContainer:proposedLineFragment:glyphPosition:characterIndex:))]
        fn bounds(&self, container: Option<&AnyObject>, fragment: CGRect, position: CGPoint, index: usize) -> CGRect {
            self.ivars().0.borrow_mut().push(Call::Bounds { container: container.is_some(), fragment, position, index });
            rect(0.0, -3.0, 15.0, 12.0)
        }

        #[unsafe(method_id(imageForBounds:textContainer:characterIndex:))]
        fn image(&self, _bounds: CGRect, container: Option<&AnyObject>, index: usize) -> Option<Retained<NSImage>> {
            self.ivars().0.borrow_mut().push(Call::Image { container: container.is_some(), index });
            Some(solid(15, 12, RED))
        }
    }

    unsafe impl NSObjectProtocol for OfTextKit1 {}
);

define_class!(
    #[unsafe(super(NSTextAttachment, objc2::runtime::NSObject))]
    #[name = "ConformanceAttachmentOfBoth"]
    #[ivars = Calls]
    struct OfBoth;

    impl OfBoth {
        #[unsafe(method(attachmentBoundsForTextContainer:proposedLineFragment:glyphPosition:characterIndex:))]
        fn bounds(&self, container: Option<&AnyObject>, fragment: CGRect, position: CGPoint, index: usize) -> CGRect {
            self.ivars().0.borrow_mut().push(Call::Bounds { container: container.is_some(), fragment, position, index });
            rect(0.0, -3.0, 15.0, 12.0)
        }

        #[unsafe(method_id(imageForBounds:textContainer:characterIndex:))]
        fn image(&self, _bounds: CGRect, container: Option<&AnyObject>, index: usize) -> Option<Retained<NSImage>> {
            self.ivars().0.borrow_mut().push(Call::Image { container: container.is_some(), index });
            Some(solid(15, 12, RED))
        }

        #[unsafe(method(attachmentBoundsForAttributes:location:textContainer:proposedLineFragment:position:))]
        fn bounds2(
            &self,
            _attributes: &AnyObject,
            _location: &AnyObject,
            container: Option<&AnyObject>,
            fragment: CGRect,
            position: CGPoint,
        ) -> CGRect {
            self.ivars().0.borrow_mut().push(Call::Bounds2 { container: container.is_some(), fragment, position });
            rect(0.0, -3.0, 25.0, 12.0)
        }

        #[unsafe(method_id(imageForBounds:attributes:location:textContainer:))]
        fn image2(
            &self,
            _bounds: CGRect,
            _attributes: &AnyObject,
            _location: &AnyObject,
            container: Option<&AnyObject>,
        ) -> Option<Retained<NSImage>> {
            self.ivars().0.borrow_mut().push(Call::Image2 { container: container.is_some() });
            Some(solid(25, 12, RED))
        }
    }

    unsafe impl NSObjectProtocol for OfBoth {}
);

fn of_textkit1(image: bool) -> Retained<OfTextKit1> {
    let this = OfTextKit1::alloc().set_ivars(Calls(RefCell::new(Vec::new())));
    // SAFETY: NSTextAttachment's initializer.
    let this: Retained<OfTextKit1> = unsafe { msg_send![super(this), init] };
    if image {
        this.setImage(Some(&solid(15, 12, RED)));
    }
    this
}

fn of_both(image: bool) -> Retained<OfBoth> {
    let this = OfBoth::alloc().set_ivars(Calls(RefCell::new(Vec::new())));
    // SAFETY: NSTextAttachment's initializer.
    let this: Retained<OfBoth> = unsafe { msg_send![super(this), init] };
    if image {
        this.setImage(Some(&solid(25, 12, RED)));
    }
    this
}

/// The calls recorded since last asked.
fn calls(recorded: &Calls) -> Vec<Call> {
    std::mem::take(&mut *recorded.0.borrow_mut())
}

/// A layout manager laying out `s` in a container 150 wide with 7 points'
/// padding (and its storage), and a text view 200 wide in TextKit 2 (5
/// points' padding).
fn textkits(
    mtm: MainThreadMarker,
    s: &NSAttributedString,
) -> (Retained<NSTextStorage>, Retained<NSLayoutManager>, Retained<NSTextView>) {
    // SAFETY: AppKit's text classes, used as they are documented.
    unsafe {
        let storage = NSTextStorage::new();
        storage.setAttributedString(s);
        let lm = NSLayoutManager::new();
        let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(150.0, 1000.0));
        tc.setLineFragmentPadding(7.0);
        lm.addTextContainer(&tc);
        storage.addLayoutManager(&lm);
        lm.ensureLayoutForTextContainer(&tc);
        let tv =
            NSTextView::initWithFrame(NSTextView::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(200.0, 60.0)));
        tv.textStorage().expect("storage").setAttributedString(s);
        let tlm = tv.textLayoutManager().expect("TextKit 2");
        tlm.ensureLayoutForRange(&tlm.textContentManager().expect("content").documentRange());
        (storage, lm, tv)
    }
}

/// What a subclass overriding an attachment's methods is asked, and told:
/// string drawing and TextKit 2 ask TextKit 2's methods where it has them
/// (before a cell, for the bounds), else TextKit 1's, as TextKit 1 does;
/// told the container (string drawing given a width makes one), a line
/// fragment as wide as the lines may be (40000 for string drawing given
/// none) and as tall as the character's font's line, and its index.
fn subclasses(mtm: MainThreadMarker) {
    let (ascent, descent) = line_metrics();
    let ab = plain("ab").size().width;
    let told = |call: &Call, container: bool, width: f64, index: usize| match *call {
        Call::Bounds { container: c, fragment: f, index: i, .. } => {
            c == container && f.size.width == width && (f.size.height - ascent - descent).abs() < 0.01 && i == index
        }
        Call::Bounds2 { container: c, fragment: f, .. } => {
            c == container && f.size.width == width && (f.size.height - ascent - descent).abs() < 0.01
        }
        Call::Image { container: c, index: i } => c == container && i == index,
        Call::Image2 { container: c } => c == container,
    };
    let is_bounds = |c: &Call| matches!(c, Call::Bounds { .. });
    let is_bounds2 = |c: &Call| matches!(c, Call::Bounds2 { .. });
    let at_start = |c: &Call| match *c {
        Call::Bounds { position: p, .. } | Call::Bounds2 { position: p, .. } => {
            p.x == 0.0 && (p.y - ascent).abs() < 0.01
        }
        _ => false,
    };

    // TextKit 1's methods only: asked by all three.
    let a = of_textkit1(true);
    let s = with_attachment(&a, "a", "b");
    close(s.size().width, ab + 15.0);
    let c = calls(a.ivars());
    assert!(!c.is_empty() && c.iter().all(|c| is_bounds(c) && told(c, false, 40000.0, 1)), "{c:?}");
    assert!(c.iter().any(at_start), "{c:?}");
    let _ = s.boundingRectWithSize_options_context(
        NSSize::new(200.0, 0.0),
        NSStringDrawingOptions::UsesLineFragmentOrigin,
        None,
    );
    let c = calls(a.ivars());
    assert!(!c.is_empty() && c.iter().all(|c| is_bounds(c) && told(c, true, 200.0, 1)), "{c:?}");
    let (_storage, lm, tv) = textkits(mtm, &s);
    let c = calls(a.ivars());
    // TextKit 1, the container's width; TextKit 2, the width less padding.
    assert!(c.iter().any(|c| told(c, true, 150.0, 1)), "{c:?}");
    assert!(c.iter().any(|c| told(c, true, 190.0, 1)), "{c:?}");
    assert!(c.iter().all(is_bounds), "{c:?}");
    assert_eq!(lm.attachmentSizeForGlyphAtIndex(1), NSSize::new(15.0, 12.0));
    let _ = snapshot(&tv, 1.0);
    let c = calls(a.ivars());
    assert!(c.iter().any(|c| matches!(c, Call::Image { container: true, index: 1 })), "{c:?}");

    // TextKit 2's as well: asked instead by string drawing and TextKit 2.
    let a = of_both(true);
    let s = with_attachment(&a, "a", "b");
    close(s.size().width, ab + 25.0);
    let c = calls(a.ivars());
    assert!(!c.is_empty() && c.iter().all(|c| is_bounds2(c) && told(c, false, 40000.0, 1)), "{c:?}");
    assert!(c.iter().any(at_start), "{c:?}");
    let rep = bitmap(100, 40);
    draw_in(&rep, |_| s.drawInRect(NSRect::new(NSPoint::new(3.0, 2.0), NSSize::new(80.0, 30.0))));
    let c = calls(a.ivars());
    assert!(c.iter().any(|c| is_bounds2(c) && told(c, true, 80.0, 1)), "{c:?}");
    assert!(c.contains(&Call::Image2 { container: true }), "{c:?}");
    assert!(!c.iter().any(|c| matches!(c, Call::Bounds { .. } | Call::Image { .. })), "{c:?}");
    let (_storage, lm, tv) = textkits(mtm, &s);
    let c = calls(a.ivars());
    assert!(c.iter().any(|c| is_bounds(c) && told(c, true, 150.0, 1)), "TextKit 1's: {c:?}");
    assert!(c.iter().any(|c| is_bounds2(c) && told(c, true, 190.0, 1)), "TextKit 2's: {c:?}");
    assert_eq!(lm.attachmentSizeForGlyphAtIndex(1), NSSize::new(15.0, 12.0));
    let _ = snapshot(&tv, 1.0);
    assert!(calls(a.ivars()).contains(&Call::Image2 { container: true }));

    // Without an image, an attachment has a cell, which TextKit 2's method
    // comes before; TextKit 1's doesn't.
    let a = of_both(false);
    close(with_attachment(&a, "a", "b").size().width, ab + 25.0);
    assert!(calls(a.ivars()).iter().all(is_bounds2));
    let a = of_textkit1(false);
    close(with_attachment(&a, "a", "b").size().width, ab + 1.0);
    assert_eq!(calls(a.ivars()), Vec::new());
}

/// A copied attachment cell knows the same attachment.
fn cell_copies(mtm: MainThreadMarker) {
    let a = NSTextAttachment::new();
    let c = NSTextAttachmentCell::initImageCell(NSTextAttachmentCell::alloc(mtm), Some(&solid(8, 9, BLUE)));
    a.setAttachmentCell(Some(ProtocolObject::from_ref(&*c)));
    let copy: Retained<NSTextAttachmentCell> = unsafe { Retained::cast_unchecked(c.copy()) };
    assert!(unsafe { copy.attachment() }.is_some_and(|x| std::ptr::eq(&*x, &*a)));
    assert_eq!(copy.cellSize(), NSSize::new(8.0, 9.0));
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("attachments", attachments),
        ("cells", cells),
        ("measuring", measuring),
        ("drawing", drawing),
        ("program_cells", program_cells),
        ("textkit1", textkit1),
        ("textkit2", textkit2),
        ("editing", editing),
        ("packages", packages),
        ("baseline_offsets", baseline_offsets),
        ("controls", controls),
        ("subclasses", subclasses),
        ("cell_copies", cell_copies),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
