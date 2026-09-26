//! Probe (temporary): NSLayoutManager on macOS.

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSFont, NSFontAttributeName, NSFontWeightRegular, NSLayoutManager, NSTextContainer, NSTextStorage};
use objc2_foundation::{NSDictionary, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize, NSString};

use sidestep as _;

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

fn setup(text: &str, width: f64) -> (Retained<NSTextStorage>, Retained<NSLayoutManager>, Retained<NSTextContainer>) {
    let font = NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular });
    let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&[unsafe { NSFontAttributeName }], &[&*font as &AnyObject]);
    let ts: Retained<NSTextStorage> = unsafe { objc2::msg_send![NSTextStorage::alloc(), initWithString: &*s(text), attributes: &*attrs] };
    let lm = NSLayoutManager::new();
    let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(width, 1.0e7));
    lm.addTextContainer(&tc);
    ts.addLayoutManager(&lm);
    (ts, lm, tc)
}

fn r(x: NSRect) -> (f64, f64, f64, f64) {
    (x.origin.x, x.origin.y, x.size.width, x.size.height)
}

#[test]
fn probe() {
    let font = NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular });
    println!("font asc {} desc {} leading {} cap {}", font.ascender(), font.descender(), font.leading(), font.capHeight());
    let (ts, lm, tc) = setup("abcd efgh\nij", 1.0e7);
    println!("padding {} size {:?} tracks {} {}", tc.lineFragmentPadding(), tc.size(), tc.widthTracksTextView(), tc.heightTracksTextView());
    println!("glyphs {} len {}", lm.numberOfGlyphs(), ts.length());
    println!("default line height {}", lm.defaultLineHeightForFont(&font));
    println!("baseline offset {}", lm.defaultBaselineOffsetForFont(&font));
    lm.ensureLayoutForTextContainer(&tc);
    println!("used {:?}", r(lm.usedRectForTextContainer(&tc)));
    let mut eff = NSRange::new(0, 0);
    let lf = unsafe { lm.lineFragmentRectForGlyphAtIndex_effectiveRange(0, &mut eff) };
    println!("lf0 {:?} eff {:?}", r(lf), (eff.location, eff.length));
    let lfu = unsafe { lm.lineFragmentUsedRectForGlyphAtIndex_effectiveRange(0, &mut eff) };
    println!("lf0 used {:?}", r(lfu));
    let lf = unsafe { lm.lineFragmentRectForGlyphAtIndex_effectiveRange(10, &mut eff) };
    println!("lf10 {:?} eff {:?}", r(lf), (eff.location, eff.length));
    println!("extra {:?} extraUsed {:?}", r(lm.extraLineFragmentRect()), r(lm.extraLineFragmentUsedRect()));
    for i in [0usize, 1, 4, 9, 10, 11] {
        println!("bound {i} {:?} loc {:?}", r(lm.boundingRectForGlyphRange_inTextContainer(NSRange::new(i, 1), &tc)), lm.locationForGlyphAtIndex(i));
    }
    println!("bound 0..3 {:?}", r(lm.boundingRectForGlyphRange_inTextContainer(NSRange::new(0, 3), &tc)));
    println!("bound 2..12 {:?}", r(lm.boundingRectForGlyphRange_inTextContainer(NSRange::new(2, 10), &tc)));
    for p in [(0.0, 0.0), (5.0, 3.0), (12.0, 5.0), (100.0, 5.0), (1.0, 20.0), (500.0, 20.0), (3.0, 100.0)] {
        let mut frac = 0.0;
        let g = unsafe { lm.glyphIndexForPoint_inTextContainer_fractionOfDistanceThroughGlyph(NSPoint::new(p.0, p.1), &tc, &mut frac) };
        let mut f2 = 0.0;
        let c = unsafe { lm.characterIndexForPoint_inTextContainer_fractionOfDistanceBetweenInsertionPoints(NSPoint::new(p.0, p.1), &tc, &mut f2) };
        println!("point {:?} glyph {g} frac {frac:.3} char {c} f2 {f2:.3}", p);
    }
    println!("range for container {:?}", lm.glyphRangeForTextContainer(&tc));
    println!("glyph range for rect {:?}", lm.glyphRangeForBoundingRect_inTextContainer(NSRect::new(NSPoint::new(0.0, 16.0), NSSize::new(10.0, 2.0)), &tc));
    println!("first unlaid {}", lm.firstUnlaidCharacterIndex());
    // Wrapping
    let (_ts2, lm2, tc2) = setup("aaaa bbbb cccc dddd", 60.0);
    lm2.ensureLayoutForTextContainer(&tc2);
    let mut i = 0;
    while i < lm2.numberOfGlyphs() {
        let mut eff = NSRange::new(0, 0);
        let lf = unsafe { lm2.lineFragmentRectForGlyphAtIndex_effectiveRange(i, &mut eff) };
        let used = unsafe { lm2.lineFragmentUsedRectForGlyphAtIndex_effectiveRange(i, std::ptr::null_mut()) };
        println!("wrap line {:?} lf {:?} used {:?}", (eff.location, eff.length), r(lf), r(used));
        i = eff.location + eff.length;
    }
    println!("wrap used {:?} extra {:?}", r(lm2.usedRectForTextContainer(&tc2)), r(lm2.extraLineFragmentRect()));
    // Empty
    let (_ts3, lm3, tc3) = setup("", 200.0);
    println!("empty used {:?} extra {:?} glyphs {}", r(lm3.usedRectForTextContainer(&tc3)), r(lm3.extraLineFragmentRect()), lm3.numberOfGlyphs());
    // Trailing newline
    let (ts4, lm4, tc4) = setup("ab\n", 200.0);
    println!("nl used {:?} extra {:?} extraUsed {:?}", r(lm4.usedRectForTextContainer(&tc4)), r(lm4.extraLineFragmentRect()), r(lm4.extraLineFragmentUsedRect()));
    // Edit
    let m: &NSMutableAttributedString = &ts4;
    m.replaceCharactersInRange_withString(NSRange::new(3, 0), &s("xyz"));
    println!("after edit used {:?} extra {:?} glyphs {}", r(lm4.usedRectForTextContainer(&tc4)), r(lm4.extraLineFragmentRect()), lm4.numberOfGlyphs());
    // Emoji glyph count
    let (_t5, lm5, _tc5) = setup("a😀é", 200.0);
    println!("emoji glyphs {} chars 4; glyph for char 2 {} char for glyph 2 {}", lm5.numberOfGlyphs(), lm5.glyphIndexForCharacterAtIndex(2), lm5.characterIndexForGlyphAtIndex(2));
    for g in 0..lm5.numberOfGlyphs() {
        println!("  prop {g} {:?}", lm5.propertyForGlyphAtIndex(g));
    }
    let mut actual = NSRange::new(0, 0);
    let gr = unsafe { lm5.glyphRangeForCharacterRange_actualCharacterRange(NSRange::new(2, 1), &mut actual) };
    println!("glyph range for char (2,1) {:?} actual {:?}", gr, actual);
}
