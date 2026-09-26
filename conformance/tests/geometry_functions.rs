//! Foundation's C geometry functions on points, sizes and rectangles, with
//! the edge cases macOS shows: empty rectangles, touching edges, rounding
//! options and the string forms.

use std::ptr::NonNull;

use objc2_foundation::{
    NSAlignmentOptions, NSContainsRect, NSDivideRect, NSEdgeInsets, NSEqualPoints, NSEqualRects, NSEqualSizes,
    NSInsetRect, NSIntegralRect, NSIntegralRectWithOptions, NSIntersectionRect, NSIntersectsRect, NSIsEmptyRect,
    NSMouseInRect, NSOffsetRect, NSPoint, NSPointFromString, NSPointInRect, NSRect, NSRectEdge, NSRectFromString,
    NSSize, NSSizeFromString, NSString, NSUnionRect,
};

use sidestep as _;

fn r(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

const ZERO: NSRect = NSRect { origin: NSPoint { x: 0.0, y: 0.0 }, size: NSSize { width: 0.0, height: 0.0 } };

#[test]
fn string_forms() {
    assert_eq!(NSString::from_rect(r(1.0, 2.5, 3.0, -4.0)).to_string(), "{{1, 2.5}, {3, -4}}");
    assert_eq!(NSString::from_rect(r(0.0, 0.0, 100.0, 22.0)).to_string(), "{{0, 0}, {100, 22}}");
    assert_eq!(NSString::from_point(NSPoint::new(1.0, 2.25)).to_string(), "{1, 2.25}");
    assert_eq!(NSString::from_size(NSSize::new(1e20, 0.1)).to_string(), "{1e+20, 0.10000000000000001}");
    assert_eq!(NSString::from_point(NSPoint::new(1.0 / 3.0, -0.0)).to_string(), "{0.33333333333333331, -0}");
    assert_eq!(
        NSString::from_point(NSPoint::new(123456789.125, 1e-7)).to_string(),
        "{123456789.125, 9.9999999999999995e-08}"
    );
    assert_eq!(NSString::from_point(NSPoint::new(f64::NAN, f64::INFINITY)).to_string(), "{nan, inf}");

    let parse = |s: &str| {
        let s = NSString::from_str(s);
        (NSRectFromString(&s), NSPointFromString(&s), NSSizeFromString(&s))
    };
    assert_eq!(parse("{{1, 2}, {3, 4}}"), (r(1.0, 2.0, 3.0, 4.0), NSPoint::new(1.0, 2.0), NSSize::new(1.0, 2.0)));
    assert_eq!(parse("1 2 3 4").0, r(1.0, 2.0, 3.0, 4.0));
    assert_eq!(parse("{1.5,2}"), (r(1.5, 2.0, 0.0, 0.0), NSPoint::new(1.5, 2.0), NSSize::new(1.5, 2.0)));
    assert_eq!(parse(""), (ZERO, NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)));
    assert_eq!(parse("x").0, ZERO);
    assert_eq!(parse("{{1,2},{3").0, r(1.0, 2.0, 3.0, 0.0));
    assert_eq!(parse("  -3.5e1 7").1, NSPoint::new(-35.0, 7.0));
    assert_eq!(parse("1,2,3,4,5").0, r(1.0, 2.0, 3.0, 4.0));
}

#[test]
fn integral_rects() {
    assert_eq!(NSIntegralRect(r(0.5, 0.5, 1.2, 1.2)), r(0.0, 0.0, 2.0, 2.0));
    assert_eq!(NSIntegralRect(r(-0.5, -1.5, 1.2, 0.0)), ZERO);
    assert_eq!(NSIntegralRect(r(0.5, 0.5, -2.0, 3.0)), ZERO);
    let a = r(0.4, 0.6, 1.2, 1.5);
    let with = |o| NSIntegralRectWithOptions(a, o);
    assert_eq!(with(NSAlignmentOptions::AlignAllEdgesNearest), r(0.0, 1.0, 2.0, 1.0));
    assert_eq!(with(NSAlignmentOptions::AlignAllEdgesOutward), r(0.0, 0.0, 2.0, 3.0));
    assert_eq!(with(NSAlignmentOptions::AlignAllEdgesInward), r(1.0, 1.0, 0.0, 1.0));
    assert_eq!(
        with(
            NSAlignmentOptions::AlignMinXOutward
                | NSAlignmentOptions::AlignWidthNearest
                | NSAlignmentOptions::AlignMinYInward
                | NSAlignmentOptions::AlignHeightNearest
        ),
        r(0.0, 1.0, 1.0, 2.0)
    );
    assert_eq!(
        with(
            NSAlignmentOptions::AlignMaxXInward
                | NSAlignmentOptions::AlignWidthOutward
                | NSAlignmentOptions::AlignMaxYNearest
                | NSAlignmentOptions::AlignHeightInward
        ),
        r(-1.0, 1.0, 2.0, 1.0)
    );
    // Halves round up, also below zero.
    let nearest = NSAlignmentOptions::AlignAllEdgesNearest;
    assert_eq!(NSIntegralRectWithOptions(r(0.5, 1.5, 2.5, 0.5), nearest), r(1.0, 2.0, 2.0, 0.0));
    assert_eq!(NSIntegralRectWithOptions(r(-0.5, -1.5, 1.0, 1.0), nearest), r(0.0, -1.0, 1.0, 1.0));
}

#[test]
fn unions_and_intersections() {
    assert_eq!(NSUnionRect(r(0.0, 0.0, 1.0, 1.0), r(5.0, 5.0, 1.0, 2.0)), r(0.0, 0.0, 6.0, 7.0));
    assert_eq!(NSUnionRect(ZERO, r(5.0, 5.0, 1.0, 2.0)), r(5.0, 5.0, 1.0, 2.0));
    assert_eq!(NSUnionRect(r(1.0, 1.0, 2.0, 2.0), r(5.0, 5.0, 0.0, 2.0)), r(1.0, 1.0, 2.0, 2.0));
    assert_eq!(NSUnionRect(r(1.0, 1.0, 0.0, 2.0), r(5.0, 5.0, 0.0, 2.0)), ZERO);
    assert_eq!(NSIntersectionRect(r(0.0, 0.0, 4.0, 4.0), r(2.0, 3.0, 5.0, 5.0)), r(2.0, 3.0, 2.0, 1.0));
    assert_eq!(NSIntersectionRect(r(0.0, 0.0, 1.0, 1.0), r(2.0, 3.0, 5.0, 5.0)), ZERO);
    assert_eq!(NSIntersectionRect(r(0.0, 0.0, 2.0, 2.0), r(2.0, 0.0, 5.0, 5.0)), ZERO);
    assert_eq!(NSIntersectionRect(r(1.0, 1.0, 0.0, 0.0), r(0.0, 0.0, 5.0, 5.0)), ZERO);
    assert!(!NSIntersectsRect(r(0.0, 0.0, 2.0, 2.0), r(2.0, 0.0, 5.0, 5.0)));
    assert!(!NSIntersectsRect(r(1.0, 1.0, 0.0, 0.0), r(0.0, 0.0, 5.0, 5.0)));
    assert!(NSIntersectsRect(r(0.0, 0.0, 2.0, 2.0), r(1.0, 1.0, 5.0, 5.0)));
    assert!(NSContainsRect(r(0.0, 0.0, 5.0, 5.0), r(1.0, 1.0, 2.0, 2.0)));
    assert!(!NSContainsRect(r(0.0, 0.0, 5.0, 5.0), r(1.0, 1.0, 0.0, 0.0)));
    assert!(NSContainsRect(r(0.0, 0.0, 5.0, 5.0), r(0.0, 0.0, 5.0, 5.0)));
    assert!(!NSContainsRect(r(0.0, 0.0, 0.0, 5.0), ZERO));
}

#[test]
fn points_and_edges() {
    let square = r(0.0, 0.0, 2.0, 2.0);
    assert!(NSPointInRect(NSPoint::new(0.0, 0.0), square));
    assert!(!NSPointInRect(NSPoint::new(2.0, 1.0), square));
    assert!(!NSPointInRect(NSPoint::new(1.0, 2.0), square));
    assert!(!NSPointInRect(NSPoint::new(0.0, 0.0), r(0.0, 0.0, 0.0, 2.0)));
    // Flipped rectangles include their top edge, others their bottom.
    assert!(NSMouseInRect(NSPoint::new(1.0, 0.0), square, true));
    assert!(!NSMouseInRect(NSPoint::new(1.0, 2.0), square, true));
    assert!(!NSMouseInRect(NSPoint::new(1.0, 0.0), square, false));
    assert!(NSMouseInRect(NSPoint::new(1.0, 2.0), square, false));
    assert!(NSIsEmptyRect(r(0.0, 0.0, -1.0, 1.0)));
    assert!(NSIsEmptyRect(r(0.0, 0.0, 0.0, 1.0)));
    assert!(!NSIsEmptyRect(r(0.0, 0.0, 1.0, 1.0)));
    assert!(NSEqualPoints(NSPoint::new(1.0, 2.0), NSPoint::new(1.0, 2.0)));
    assert!(!NSEqualPoints(NSPoint::new(f64::NAN, 0.0), NSPoint::new(f64::NAN, 0.0)));
    assert!(!NSEqualSizes(NSSize::new(1.0, 2.0), NSSize::new(1.0, 2.5)));
    assert!(NSEqualRects(square, r(0.0, 0.0, 2.0, 2.0)));
    let insets = NSEdgeInsets { top: 1.0, left: 2.0, bottom: 3.0, right: 4.0 };
    assert!(insets.equal(insets));
    assert!(!insets.equal(NSEdgeInsets { top: 0.0, ..insets }));
}

#[test]
fn insets_offsets_and_division() {
    let ten = r(0.0, 0.0, 10.0, 10.0);
    assert_eq!(NSInsetRect(ten, 2.0, 3.0), r(2.0, 3.0, 6.0, 4.0));
    assert_eq!(NSInsetRect(ten, -2.0, -3.0), r(-2.0, -3.0, 14.0, 16.0));
    assert_eq!(NSInsetRect(ten, 6.0, 1.0), r(6.0, 1.0, -2.0, 8.0));
    assert_eq!(NSOffsetRect(ten, 2.0, -3.0), r(2.0, -3.0, 10.0, 10.0));
    let divide = |amount: f64, edge: NSRectEdge| {
        let (mut slice, mut rem) = (ZERO, ZERO);
        unsafe {
            NSDivideRect(r(0.0, 0.0, 10.0, 20.0), NonNull::from(&mut slice), NonNull::from(&mut rem), amount, edge)
        };
        (slice, rem)
    };
    assert_eq!(divide(3.0, NSRectEdge::MinX), (r(0.0, 0.0, 3.0, 20.0), r(3.0, 0.0, 7.0, 20.0)));
    assert_eq!(divide(30.0, NSRectEdge::MinX), (r(0.0, 0.0, 10.0, 20.0), r(10.0, 0.0, 0.0, 20.0)));
    assert_eq!(divide(-3.0, NSRectEdge::MinX), (r(0.0, 0.0, -3.0, 20.0), r(-3.0, 0.0, 13.0, 20.0)));
    assert_eq!(divide(3.0, NSRectEdge::MinY), (r(0.0, 0.0, 10.0, 3.0), r(0.0, 3.0, 10.0, 17.0)));
    assert_eq!(divide(3.0, NSRectEdge::MaxX), (r(7.0, 0.0, 3.0, 20.0), r(0.0, 0.0, 7.0, 20.0)));
    assert_eq!(divide(30.0, NSRectEdge::MaxX), (r(0.0, 0.0, 10.0, 20.0), r(0.0, 0.0, 0.0, 20.0)));
    assert_eq!(divide(-3.0, NSRectEdge::MaxX), (r(13.0, 0.0, -3.0, 20.0), r(0.0, 0.0, 13.0, 20.0)));
    assert_eq!(divide(3.0, NSRectEdge::MaxY), (r(0.0, 17.0, 10.0, 3.0), r(0.0, 0.0, 10.0, 17.0)));
    assert_eq!(divide(-3.0, NSRectEdge::MaxY), (r(0.0, 23.0, 10.0, -3.0), r(0.0, 0.0, 10.0, 23.0)));
}
