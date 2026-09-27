//! NSLayoutManager and NSTextContainer, built by hand as TextKit 1
//! (storage, layout manager, container): glyphs and characters, line
//! fragments and used rects, the extra line fragment, hit testing,
//! bounding rects, wrapping, paragraph spacing, and layout after edits.
//!
//! Text is in the 12-point monospaced system font, so positions are
//! multiples of one advance; the checks compare against that advance and
//! the layout manager's own default line height, never against numbers
//! that depend on a platform's fonts. They run on the test harness's
//! worker threads: layout needs no main thread.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{AnyThread, DefinedClass, define_class};
use objc2_app_kit::{
    NSFont, NSFontAttributeName, NSFontWeightRegular, NSGlyphProperty, NSLayoutManager, NSLayoutManagerDelegate,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSTextContainer, NSTextStorage,
};
use objc2_foundation::{
    NSDictionary, NSMutableAttributedString, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString,
};

use sidestep as _;

const WIDE: f64 = 1.0e7;

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

/// The font the tests lay out with, made one test at a time: on CI's
/// macOS runner, AppKit once gave no font to one of several test threads
/// asking for it at once.
fn font() -> Retained<NSFont> {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular })
}

struct Kit {
    ts: Retained<NSTextStorage>,
    lm: Retained<NSLayoutManager>,
    tc: Retained<NSTextContainer>,
}

fn kit(text: &str, width: f64) -> Kit {
    let f = font();
    let attrs =
        NSDictionary::<NSString, AnyObject>::from_slices(&[unsafe { NSFontAttributeName }], &[&*f as &AnyObject]);
    let ts: Retained<NSTextStorage> =
        unsafe { objc2::msg_send![NSTextStorage::alloc(), initWithString: &*s(text), attributes: &*attrs] };
    let lm = NSLayoutManager::new();
    let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(width, WIDE));
    lm.addTextContainer(&tc);
    ts.addLayoutManager(&lm);
    Kit { ts, lm, tc }
}

impl Kit {
    fn h(&self) -> f64 {
        self.lm.defaultLineHeightForFont(&font())
    }

    /// One advance of the monospaced font.
    fn adv(&self) -> f64 {
        self.lm.boundingRectForGlyphRange_inTextContainer(NSRange::new(0, 1), &self.tc).size.width
    }

    fn fragment(&self, i: usize) -> (NSRect, (usize, usize)) {
        let mut r = NSRange::new(0, 0);
        let rect = unsafe { self.lm.lineFragmentRectForGlyphAtIndex_effectiveRange(i, &mut r) };
        (rect, (r.location, r.length))
    }

    fn used(&self, i: usize) -> NSRect {
        unsafe { self.lm.lineFragmentUsedRectForGlyphAtIndex_effectiveRange(i, std::ptr::null_mut()) }
    }

    fn bound(&self, loc: usize, len: usize) -> NSRect {
        self.lm.boundingRectForGlyphRange_inTextContainer(NSRange::new(loc, len), &self.tc)
    }

    fn glyph_at(&self, x: f64, y: f64) -> (usize, f64) {
        let mut f = 0.0;
        let g = unsafe {
            self.lm.glyphIndexForPoint_inTextContainer_fractionOfDistanceThroughGlyph(
                NSPoint::new(x, y),
                &self.tc,
                &mut f,
            )
        };
        (g, f)
    }

    fn replace(&self, loc: usize, len: usize, text: &str) {
        let m: &NSMutableAttributedString = &self.ts;
        m.replaceCharactersInRange_withString(NSRange::new(loc, len), &s(text));
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.01
}

#[track_caller]
fn assert_rect(r: NSRect, x: f64, y: f64, w: f64, h: f64) {
    let ok = close(r.origin.x, x) && close(r.origin.y, y) && close(r.size.width, w) && close(r.size.height, h);
    assert!(ok, "rect {r:?} is not ({x}, {y}, {w}, {h})");
}

#[test]
fn a_container_starts_padded_and_unbounded_by_tracking() {
    let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(200.0, 100.0));
    assert_eq!(tc.lineFragmentPadding(), 5.0);
    assert_eq!(tc.size(), NSSize::new(200.0, 100.0));
    assert!(!tc.widthTracksTextView() && !tc.heightTracksTextView());
    assert_eq!(tc.maximumNumberOfLines(), 0);
    assert!(tc.isSimpleRectangularTextContainer());
    let lm = NSLayoutManager::new();
    lm.addTextContainer(&tc);
    assert!(unsafe { tc.layoutManager() }.is_some_and(|m| std::ptr::eq(&*m, &*lm)));
    assert_eq!(lm.textContainers().count(), 1);
    assert!(lm.usesFontLeading());
    assert!(!lm.allowsNonContiguousLayout());
}

/// A glyph for each UTF-16 unit; the second half of a pair is a null
/// glyph, and ranges grow to whole characters.
#[test]
fn glyphs_are_characters() {
    let k = kit("a😀é", 200.0);
    assert_eq!(k.lm.numberOfGlyphs(), 4);
    assert_eq!(k.lm.glyphIndexForCharacterAtIndex(2), 2);
    assert_eq!(k.lm.characterIndexForGlyphAtIndex(3), 3);
    assert_eq!(k.lm.propertyForGlyphAtIndex(0), NSGlyphProperty(0));
    assert_eq!(k.lm.propertyForGlyphAtIndex(2), NSGlyphProperty::Null);
    let mut actual = NSRange::new(0, 0);
    let g = unsafe { k.lm.glyphRangeForCharacterRange_actualCharacterRange(NSRange::new(2, 1), &mut actual) };
    assert_eq!((g, actual), (NSRange::new(1, 2), NSRange::new(1, 2)));
}

/// Line fragments are as wide as the container and a line tall; used
/// rects hold the text and the padding at each end.
#[test]
fn line_fragments_and_used_rects() {
    let k = kit("abcd efgh\nij", WIDE);
    k.lm.ensureLayoutForTextContainer(&k.tc);
    assert_eq!(k.lm.firstUnlaidCharacterIndex(), 12);
    let (h, adv) = (k.h(), k.adv());
    assert!(h > 0.0 && adv > 0.0);
    let (r0, e0) = k.fragment(0);
    assert_rect(r0, 0.0, 0.0, WIDE, h);
    assert_eq!(e0, (0, 10));
    let (r1, e1) = k.fragment(10);
    assert_rect(r1, 0.0, h, WIDE, h);
    assert_eq!(e1, (10, 2));
    assert_rect(k.used(0), 0.0, 0.0, 9.0 * adv + 10.0, h);
    assert_rect(k.used(11), 0.0, h, 2.0 * adv + 10.0, h);
    assert_rect(k.lm.usedRectForTextContainer(&k.tc), 0.0, 0.0, 9.0 * adv + 10.0, 2.0 * h);
    // No extra line fragment: the text doesn't end in a newline.
    assert_eq!(k.lm.extraLineFragmentRect().size, NSSize::new(0.0, 0.0));
    // Glyphs sit one advance apart after the padding, on the baseline.
    let baseline = k.lm.defaultBaselineOffsetForFont(&font());
    for i in [0usize, 1, 4, 8] {
        assert_rect(k.bound(i, 1), 5.0 + i as f64 * adv, 0.0, adv, h);
        let loc = k.lm.locationForGlyphAtIndex(i);
        assert!(close(loc.x, 5.0 + i as f64 * adv) && close(loc.y, baseline), "location {loc:?}");
    }
    assert_rect(k.bound(10, 1), 5.0, h, adv, h);
    // The newline reaches to the end of the line, short of the padding.
    assert_rect(k.bound(9, 1), 5.0 + 9.0 * adv, 0.0, WIDE - 10.0 - 9.0 * adv, h);
    assert_rect(k.bound(0, 3), 5.0, 0.0, 3.0 * adv, h);
    assert_rect(k.bound(2, 10), 5.0, 0.0, WIDE - 10.0, 2.0 * h);
    assert_eq!(k.lm.glyphRangeForTextContainer(&k.tc), NSRange::new(0, 12));
}

#[test]
fn hit_testing() {
    let k = kit("abcd efgh\nij", WIDE);
    k.lm.ensureLayoutForTextContainer(&k.tc);
    let (h, adv) = (k.h(), k.adv());
    let (g, f) = k.glyph_at(5.0 + 1.5 * adv, h / 2.0);
    assert!(g == 1 && close(f, 0.5), "{g} {f}");
    assert_eq!(k.glyph_at(0.0, 0.0), (0, 0.0));
    // Past a line's end: its last glyph, the newline, all the way through.
    assert_eq!(k.glyph_at(500.0, h / 2.0), (9, 1.0));
    assert_eq!(k.glyph_at(1.0, h + 2.0).0, 10);
    assert_eq!(k.glyph_at(500.0, h + 2.0), (11, 1.0));
    // Below the text: the last glyph.
    assert_eq!(k.glyph_at(3.0, 10.0 * h), (11, 1.0));
    let mut f = 0.0;
    let c = unsafe {
        k.lm.characterIndexForPoint_inTextContainer_fractionOfDistanceBetweenInsertionPoints(
            NSPoint::new(5.0 + 2.25 * adv, h / 2.0),
            &k.tc,
            &mut f,
        )
    };
    assert!(c == 2 && close(f, 0.25), "{c} {f}");
    // The glyphs of the lines a rect meets.
    let r = NSRect::new(NSPoint::new(0.0, h + 1.0), NSSize::new(10.0, 2.0));
    assert_eq!(k.lm.glyphRangeForBoundingRect_inTextContainer(r, &k.tc), NSRange::new(10, 2));
}

/// Words wrap at the container's width less its padding; a line keeps
/// its trailing space.
#[test]
fn wrapping() {
    let probe = kit("a", WIDE);
    let adv = probe.adv();
    let k = kit("aaaa bbbb cccc dddd", 10.0 + 7.5 * adv);
    k.lm.ensureLayoutForTextContainer(&k.tc);
    let h = k.h();
    let mut lines = Vec::new();
    let mut i = 0;
    while i < k.lm.numberOfGlyphs() {
        let (r, e) = k.fragment(i);
        lines.push((e, r.origin.y));
        i = e.0 + e.1;
    }
    assert_eq!(lines, [((0, 5), 0.0), ((5, 5), h), ((10, 5), 2.0 * h), ((15, 4), 3.0 * h)]);
    assert_rect(k.used(0), 0.0, 0.0, 5.0 * adv + 10.0, h);
    assert_rect(k.used(16), 0.0, 3.0 * h, 4.0 * adv + 10.0, h);
    assert_rect(k.lm.usedRectForTextContainer(&k.tc), 0.0, 0.0, 5.0 * adv + 10.0, 4.0 * h);
    // enumerateLineFragmentsForGlyphRange: says the same.
    let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let block = RcBlock::new(
        move |rect: NSRect, used: NSRect, _c: NonNull<NSTextContainer>, r: NSRange, _stop: NonNull<Bool>| {
            sink.borrow_mut().push((rect.origin.y, used.size.width, r.location, r.length));
        },
    );
    k.lm.enumerateLineFragmentsForGlyphRange_usingBlock(NSRange::new(3, 10), &block);
    let seen = seen.take();
    assert_eq!(seen.len(), 3);
    assert_eq!((seen[0].2, seen[0].3, seen[2].2), (0, 5, 10));
    assert!(close(seen[1].0, h) && close(seen[1].1, 5.0 * adv + 10.0));
}

/// Text that is empty or ends in a newline ends in the extra line
/// fragment: as wide as the container, a line tall, with padding as its
/// used width.
#[test]
fn the_extra_line_fragment() {
    let k = kit("", 200.0);
    k.lm.ensureLayoutForTextContainer(&k.tc);
    let extra = k.lm.extraLineFragmentRect();
    assert_eq!((extra.origin.x, extra.origin.y, extra.size.width), (0.0, 0.0, 200.0));
    assert!(extra.size.height > 0.0);
    let used = k.lm.extraLineFragmentUsedRect();
    assert_eq!(used.size.width, 10.0);
    assert!(k.lm.extraLineFragmentTextContainer().is_some());
    assert_eq!(k.lm.usedRectForTextContainer(&k.tc).size.width, 10.0);
    let k = kit("ab\n", 200.0);
    k.lm.ensureLayoutForTextContainer(&k.tc);
    let (h, extra) = (k.h(), k.lm.extraLineFragmentRect());
    assert!(close(extra.origin.y, h) && extra.size.width == 200.0 && close(extra.size.height, h), "{extra:?}");
    assert_eq!(k.lm.usedRectForTextContainer(&k.tc).size.height, 2.0 * h);
}

/// Paragraph spacing belongs to the line fragments around it.
#[test]
fn paragraph_spacing() {
    let k = kit("ab\ncd\nef", WIDE);
    let style = NSMutableParagraphStyle::new();
    style.setParagraphSpacing(10.0);
    let m: &NSMutableAttributedString = &k.ts;
    unsafe { m.addAttribute_value_range(NSParagraphStyleAttributeName, &style, NSRange::new(3, 3)) };
    k.lm.ensureLayoutForTextContainer(&k.tc);
    let h = k.h();
    let (r0, _) = k.fragment(0);
    let (r1, _) = k.fragment(3);
    let (r2, _) = k.fragment(6);
    assert_rect(r0, 0.0, 0.0, WIDE, h);
    assert_rect(r1, 0.0, h, WIDE, h + 10.0);
    assert_rect(r2, 0.0, 2.0 * h + 10.0, WIDE, h);
    assert_rect(k.used(3), 0.0, h, 2.0 * 0.0 + k.used(3).size.width, h);
}

/// Laying out again after edits gives what laying out afresh does.
#[test]
fn edits_lay_out_as_fresh_text_does() {
    let k = kit("one two three\nfour five\nsix", 120.0);
    k.lm.ensureLayoutForTextContainer(&k.tc);
    k.replace(4, 3, "TWO AND MORE WORDS");
    k.replace(0, 0, "zero\n");
    k.replace(k.ts.length(), 0, "\nseven");
    k.replace(9, 1, "");
    let text = k.ts.string().to_string();
    k.lm.ensureLayoutForTextContainer(&k.tc);
    let fresh = kit(&text, 120.0);
    fresh.lm.ensureLayoutForTextContainer(&fresh.tc);
    let fragments = |k: &Kit| {
        let mut out = Vec::new();
        let mut i = 0;
        while i < k.lm.numberOfGlyphs() {
            let (r, e) = k.fragment(i);
            out.push((e, r.origin.y, k.used(i).size.width));
            i = e.0 + e.1;
        }
        out
    };
    assert_eq!(fragments(&k), fragments(&fresh));
    assert_eq!(k.lm.usedRectForTextContainer(&k.tc), fresh.lm.usedRectForTextContainer(&fresh.tc));
    // A narrower container lays the text out again.
    k.tc.setSize(NSSize::new(60.0, WIDE));
    fresh.tc.setSize(NSSize::new(60.0, WIDE));
    k.lm.ensureLayoutForTextContainer(&k.tc);
    fresh.lm.ensureLayoutForTextContainer(&fresh.tc);
    assert_eq!(fragments(&k), fragments(&fresh));
}

/// A kit whose text has one paragraph style throughout, laid out.
fn styled(text: &str, width: f64, f: &dyn Fn(&NSMutableParagraphStyle)) -> Kit {
    let k = kit(text, width);
    let style = NSMutableParagraphStyle::new();
    f(&style);
    let m: &NSMutableAttributedString = &k.ts;
    unsafe { m.addAttribute_value_range(NSParagraphStyleAttributeName, &style, NSRange::new(0, k.ts.length())) };
    k.lm.ensureLayoutForTextContainer(&k.tc);
    k
}

/// Used rects are where alignment and indents put the text, and the
/// container's is their union.
#[test]
fn used_rects_follow_alignment_and_indents() {
    use objc2_app_kit::NSTextAlignment;
    let k = styled("ab\ncdef", 200.0, &|s| s.setAlignment(NSTextAlignment::Center));
    let (h, adv) = (k.h(), k.adv());
    assert_rect(k.used(0), (190.0 - 2.0 * adv) / 2.0, 0.0, 2.0 * adv + 10.0, h);
    assert_rect(k.used(3), (190.0 - 4.0 * adv) / 2.0, h, 4.0 * adv + 10.0, h);
    assert_rect(k.lm.usedRectForTextContainer(&k.tc), (190.0 - 4.0 * adv) / 2.0, 0.0, 4.0 * adv + 10.0, 2.0 * h);
    let k = styled("ab\ncdef", 200.0, &|s| s.setAlignment(NSTextAlignment::Right));
    assert_rect(k.used(0), 190.0 - 2.0 * adv, 0.0, 2.0 * adv + 10.0, h);
    assert_rect(k.lm.usedRectForTextContainer(&k.tc), 190.0 - 4.0 * adv, 0.0, 4.0 * adv + 10.0, 2.0 * h);
    // Indents: the first line's, then the rest's.
    let k = styled("ab cd ef gh ij kl", 100.0, &|s| {
        s.setFirstLineHeadIndent(20.0);
        s.setHeadIndent(10.0);
    });
    assert_eq!(k.fragment(0).1, (0, 9));
    assert_rect(k.used(0), 20.0, 0.0, 9.0 * adv + 10.0, h);
    assert_rect(k.used(9), 10.0, h, 8.0 * adv + 10.0, h);
    assert_rect(k.lm.usedRectForTextContainer(&k.tc), 10.0, 0.0, 9.0 * adv + 20.0, 2.0 * h);
    // The extra line fragment is aligned as well.
    let k = styled("ab\n", 200.0, &|s| s.setAlignment(NSTextAlignment::Center));
    assert_rect(k.lm.extraLineFragmentUsedRect(), 95.0, h, 10.0, h);
    assert_rect(k.lm.usedRectForTextContainer(&k.tc), (190.0 - 2.0 * adv) / 2.0, 0.0, 2.0 * adv + 10.0, 2.0 * h);
}

/// A used rect reaches no further than its line may: a clipped line,
/// spaces hanging past a wrap or past the right edge of right-aligned
/// text, and a positive tail indent stop it.
#[test]
fn used_rects_stay_inside_their_line() {
    use objc2_app_kit::{NSLineBreakMode, NSTextAlignment};
    let k = styled("abcdefghijklmnopqrst", 60.0, &|s| s.setLineBreakMode(NSLineBreakMode::ByClipping));
    let (h, adv) = (k.h(), k.adv());
    assert_eq!(k.fragment(0).1, (0, 20));
    assert_rect(k.used(0), 0.0, 0.0, 60.0, h);
    assert_rect(k.lm.usedRectForTextContainer(&k.tc), 0.0, 0.0, 60.0, h);
    // (How many of the spaces stay on the first line is the line
    // breaker's business.)
    let k = styled("abcdef      ghi", 60.0, &|_| {});
    assert_rect(k.used(0), 0.0, 0.0, 60.0, h);
    let k = styled("ab cd ef gh ij", 60.0, &|s| s.setAlignment(NSTextAlignment::Right));
    assert_eq!(k.fragment(0).1, (0, 6));
    assert_rect(k.used(0), 50.0 - 5.0 * adv, 0.0, 5.0 * adv + 10.0, h);
    let k = styled("ab cd ef gh ij kl", 100.0, &|s| s.setTailIndent(60.0));
    assert_eq!(k.fragment(0).1, (0, 9));
    assert_rect(k.used(0), 0.0, 0.0, 70.0, h);
    let k = styled("ab cd ef gh ij kl", 100.0, &|s| s.setTailIndent(-20.0));
    assert_rect(k.used(0), 0.0, 0.0, 9.0 * adv + 10.0, h);
}

/// Line spacing belongs to the fragment above it, used rect too, but not
/// after the text's last line; minimum line heights and multiples make
/// lines taller.
#[test]
fn line_spacing_and_heights() {
    let k = styled("ab\ncd", 200.0, &|s| s.setLineSpacing(4.0));
    let h = k.h();
    assert_rect(k.fragment(0).0, 0.0, 0.0, 200.0, h + 4.0);
    assert!(close(k.used(0).size.height, h + 4.0), "{:?}", k.used(0));
    assert_rect(k.fragment(3).0, 0.0, h + 4.0, 200.0, h);
    assert!(close(k.lm.usedRectForTextContainer(&k.tc).size.height, 2.0 * h + 4.0));
    let k = styled("ab\ncd", 200.0, &|s| s.setMinimumLineHeight(30.0));
    assert_rect(k.fragment(0).0, 0.0, 0.0, 200.0, 30.0);
    assert_rect(k.fragment(3).0, 0.0, 30.0, 200.0, 30.0);
    let k = styled("ab\ncd", 200.0, &|s| s.setLineHeightMultiple(2.0));
    assert_rect(k.fragment(3).0, 0.0, 2.0 * h, 200.0, 2.0 * h);
}

/// A container with a most lines holds only those: the text after them
/// has no line fragments.
#[test]
fn a_container_limits_its_lines() {
    let k = kit("ab\ncd\nef\ngh", 200.0);
    k.tc.setMaximumNumberOfLines(2);
    k.lm.ensureLayoutForTextContainer(&k.tc);
    let h = k.h();
    assert_rect(k.fragment(3).0, 0.0, h, 200.0, h);
    assert_rect(k.fragment(6).0, 0.0, 0.0, 0.0, 0.0);
    assert!(close(k.lm.usedRectForTextContainer(&k.tc).size.height, 2.0 * h));
    let k = kit("aaaa bbbb cccc dddd", 50.0);
    k.tc.setMaximumNumberOfLines(2);
    k.lm.ensureLayoutForTextContainer(&k.tc);
    assert_rect(k.fragment(12).0, 0.0, 0.0, 0.0, 0.0);
    assert!(close(k.lm.usedRectForTextContainer(&k.tc).size.height, 2.0 * h));
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextLayoutTestDelegate"]
    #[ivars = RefCell<Vec<bool>>]
    struct LayoutDelegate;

    unsafe impl NSObjectProtocol for LayoutDelegate {}

    unsafe impl NSLayoutManagerDelegate for LayoutDelegate {
        #[unsafe(method(layoutManager:didCompleteLayoutForTextContainer:atEnd:))]
        fn did_complete(&self, _lm: &NSLayoutManager, _c: Option<&NSTextContainer>, at_end: bool) {
            self.ivars().borrow_mut().push(at_end);
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextLayoutTestLeavingDelegate"]
    #[ivars = RefCell<u32>]
    struct LeavingDelegate;

    unsafe impl NSObjectProtocol for LeavingDelegate {}

    unsafe impl NSLayoutManagerDelegate for LeavingDelegate {
        #[unsafe(method(layoutManagerDidInvalidateLayout:))]
        fn did_invalidate(&self, lm: &NSLayoutManager) {
            *self.ivars().borrow_mut() += 1;
            lm.setDelegate(None);
        }
    }
);

/// A delegate may let go of the layout manager while it hears from it.
#[test]
fn a_delegate_may_leave_while_told() {
    let k = kit("one\ntwo", 200.0);
    let d: Retained<LeavingDelegate> =
        unsafe { objc2::msg_send![super(LeavingDelegate::alloc().set_ivars(RefCell::default())), init] };
    k.lm.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*d)));
    k.lm.ensureLayoutForTextContainer(&k.tc);
    k.replace(0, 0, "zero\n");
    assert_eq!(*d.ivars().borrow(), 1);
    assert!(k.lm.delegate().is_none());
    k.replace(0, 0, "again\n");
    assert_eq!(*d.ivars().borrow(), 1);
}

/// The delegate hears when layout of the text is done.
#[test]
fn the_delegate_hears_layout_complete() {
    let k = kit("one\ntwo\nthree", 200.0);
    let d: Retained<LayoutDelegate> =
        unsafe { objc2::msg_send![super(LayoutDelegate::alloc().set_ivars(RefCell::default())), init] };
    k.lm.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*d)));
    k.lm.ensureLayoutForTextContainer(&k.tc);
    assert_eq!(d.ivars().borrow().last(), Some(&true), "{:?}", d.ivars().borrow());
    k.lm.setDelegate(None);
}

/// Temporary attributes: runs over character ranges, kept apart from the
/// text storage; an edit keeps what is around it and moves what follows,
/// and text typed into a run has none.
#[test]
fn temporary_attributes() {
    let red = objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    let (bg, fg) =
        unsafe { (objc2_app_kit::NSBackgroundColorAttributeName, objc2_app_kit::NSForegroundColorAttributeName) };
    let runs = |k: &Kit| {
        let mut out = Vec::new();
        let mut i = 0;
        while i < k.ts.length() {
            let mut r = NSRange::new(0, 0);
            let d = unsafe { k.lm.temporaryAttributesAtCharacterIndex_effectiveRange(i, &mut r) };
            let mut keys: Vec<String> = d.allKeys().iter().map(|k| k.to_string()).collect();
            keys.sort();
            out.push(((r.location, r.length), keys.join("+")));
            i = r.location + r.length.max(1);
        }
        out
    };
    let run = |loc: usize, len: usize, keys: &str| ((loc, len), keys.to_string());
    type Run = ((usize, usize), &'static str);
    let edits: &[((usize, usize), &str, &[Run])] = &[
        (
            (3, 0),
            "XX",
            &[((0, 2), ""), ((2, 1), "NSBackgroundColor"), ((3, 2), ""), ((5, 3), "NSBackgroundColor"), ((8, 4), "")],
        ),
        ((2, 0), "XX", &[((0, 4), ""), ((4, 4), "NSBackgroundColor"), ((8, 4), "")]),
        ((6, 0), "XX", &[((0, 2), ""), ((2, 4), "NSBackgroundColor"), ((6, 6), "")]),
        ((1, 3), "", &[((0, 1), ""), ((1, 2), "NSBackgroundColor"), ((3, 4), "")]),
        ((0, 10), "new text!!", &[((0, 10), "")]),
    ];
    for &(edit, with, want) in edits {
        let k = kit("0123456789", 200.0);
        unsafe { k.lm.addTemporaryAttribute_value_forCharacterRange(bg, &red, NSRange::new(2, 4)) };
        k.replace(edit.0, edit.1, with);
        let want: Vec<_> = want.iter().map(|&((l, n), keys)| run(l, n, keys)).collect();
        assert_eq!(runs(&k), want, "edit {edit:?} to {with:?}");
    }
    let k = kit("0123456789", 200.0);
    unsafe { k.lm.addTemporaryAttribute_value_forCharacterRange(bg, &red, NSRange::new(2, 4)) };
    unsafe { k.lm.addTemporaryAttribute_value_forCharacterRange(fg, &red, NSRange::new(4, 4)) };
    assert_eq!(
        runs(&k),
        [
            run(0, 2, ""),
            run(2, 2, "NSBackgroundColor"),
            run(4, 2, "NSBackgroundColor+NSColor"),
            run(6, 2, "NSColor"),
            run(8, 2, "")
        ]
    );
    k.lm.removeTemporaryAttribute_forCharacterRange(bg, NSRange::new(3, 2));
    assert_eq!(
        runs(&k),
        [
            run(0, 2, ""),
            run(2, 1, "NSBackgroundColor"),
            run(3, 1, ""),
            run(4, 1, "NSColor"),
            run(5, 1, "NSBackgroundColor+NSColor"),
            run(6, 2, "NSColor"),
            run(8, 2, "")
        ]
    );
    let empty = NSDictionary::<NSString, AnyObject>::new();
    unsafe { k.lm.setTemporaryAttributes_forCharacterRange(&empty, NSRange::new(0, 5)) };
    assert_eq!(runs(&k)[0], run(0, 5, ""));
    let mut r = NSRange::new(0, 0);
    let v = unsafe { k.lm.temporaryAttribute_atCharacterIndex_effectiveRange(fg, 6, &mut r) };
    assert!(v.is_some() && (r.location, r.length) == (6, 2));
    let v = unsafe { k.lm.temporaryAttribute_atCharacterIndex_effectiveRange(bg, 0, &mut r) };
    assert!(v.is_none() && (r.location, r.length) == (0, 5));
    let v = unsafe {
        k.lm.temporaryAttribute_atCharacterIndex_longestEffectiveRange_inRange(fg, 6, &mut r, NSRange::new(5, 2))
    };
    assert!(v.is_some() && (r.location, r.length) == (5, 2));
    let d = unsafe { k.lm.temporaryAttributesAtCharacterIndex_effectiveRange(10, std::ptr::null_mut()) };
    assert_eq!(d.count(), 0);
}
