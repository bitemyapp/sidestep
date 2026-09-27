//! TextKit 2 without a view: locations and ranges, a content storage's
//! elements and edits, the layout manager's fragments and line fragments,
//! custom fragments and paragraphs from delegates, the viewport
//! controller, and selections. Expected values are what macOS does.
//!
//! Layout depends on fonts, which differ between platforms (DejaVu Sans
//! stands in for Helvetica on Linux CI), so these tests check how
//! fragments and lines relate (where each goes, what spacing lies between
//! them, what ranges they cover), not their widths.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSFont, NSFontAttributeName, NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSTextContainer,
    NSTextContentManager, NSTextContentManagerDelegate, NSTextContentManagerEnumerationOptions, NSTextContentStorage,
    NSTextContentStorageDelegate, NSTextElement, NSTextElementProvider, NSTextLayoutFragment,
    NSTextLayoutFragmentEnumerationOptions, NSTextLayoutManager, NSTextLayoutManagerDelegate,
    NSTextLayoutManagerSegmentOptions, NSTextLayoutManagerSegmentType, NSTextLocation, NSTextParagraph, NSTextRange,
    NSTextSelection, NSTextSelectionAffinity, NSTextSelectionDataSource, NSTextSelectionGranularity,
    NSTextSelectionNavigationModifier, NSTextStorage, NSTextStorageObserving, NSTextViewportLayoutController,
    NSTextViewportLayoutControllerDelegate,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSComparisonResult, NSDictionary, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize,
    NSString,
};

use sidestep as _;

// Helpers.

fn font() -> Retained<NSFont> {
    NSFont::systemFontOfSize(12.0)
}

fn attributed(text: &str, style: Option<&NSMutableParagraphStyle>) -> Retained<NSAttributedString> {
    let f = font();
    let attrs = match style {
        // SAFETY: the keys are AppKit's constants.
        Some(s) => unsafe {
            NSDictionary::<NSString, AnyObject>::from_slices(
                &[NSFontAttributeName, NSParagraphStyleAttributeName],
                &[&*f as &AnyObject, &**s as &AnyObject],
            )
        },
        // SAFETY: the key is AppKit's constant.
        None => unsafe {
            NSDictionary::<NSString, AnyObject>::from_slices(&[NSFontAttributeName], &[&*f as &AnyObject])
        },
    };
    // SAFETY: an attribute dictionary.
    unsafe { NSAttributedString::new_with_attributes(&NSString::from_str(text), &attrs) }
}

fn storage_with(text: &str, style: Option<&NSMutableParagraphStyle>) -> Retained<NSTextStorage> {
    let s = NSTextStorage::new();
    s.setAttributedString(&attributed(text, style));
    s
}

struct Doc {
    cs: Retained<NSTextContentStorage>,
    tlm: Retained<NSTextLayoutManager>,
    container: Retained<NSTextContainer>,
}

impl Doc {
    fn new(text: &str, width: f64) -> Doc {
        Doc::styled(text, width, None)
    }

    fn styled(text: &str, width: f64, style: Option<&NSMutableParagraphStyle>) -> Doc {
        let cs = NSTextContentStorage::new();
        let tlm = NSTextLayoutManager::new();
        let container = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(width, 1.0e7));
        tlm.setTextContainer(Some(&container));
        cs.addTextLayoutManager(&tlm);
        cs.setTextStorage(Some(&storage_with(text, style)));
        Doc { cs, tlm, container }
    }

    fn cm(&self) -> &NSTextContentManager {
        &self.cs
    }

    fn at(&self, i: isize) -> Retained<ProtocolObject<dyn NSTextLocation>> {
        self.cs.locationFromLocation_withOffset(&self.cs.documentRange().location(), i).expect("a location in the text")
    }

    fn off(&self, l: &ProtocolObject<dyn NSTextLocation>) -> isize {
        off(self.cm(), l)
    }

    fn range(&self, a: isize, b: isize) -> Retained<NSTextRange> {
        NSTextRange::initWithLocation_endLocation(NSTextRange::alloc(), &self.at(a), Some(&self.at(b)))
            .expect("a range")
    }

    fn fragments(
        &self,
        opts: NSTextLayoutFragmentEnumerationOptions,
        from: Option<isize>,
    ) -> (Vec<Frag>, Option<isize>) {
        let out = RefCell::new(Vec::new());
        let cm = self.cm();
        let block = RcBlock::new(|f: NonNull<NSTextLayoutFragment>| -> Bool {
            // SAFETY: alive for the call.
            let f = unsafe { f.as_ref() };
            out.borrow_mut().push(frag(cm, f));
            Bool::YES
        });
        let from = from.map(|i| self.at(i));
        let ret = self.tlm.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(from.as_deref(), opts, &block);
        drop(block);
        (out.into_inner(), ret.map(|l| self.off(&l)))
    }

    fn laid(&self) -> Vec<Frag> {
        self.fragments(NSTextLayoutFragmentEnumerationOptions::EnsuresLayout, None).0
    }

    /// The elements enumerated (kept, so that comparing them compares
    /// objects, not addresses a new object may reuse) with their ranges.
    #[allow(clippy::type_complexity)]
    fn elements(
        &self,
        from: Option<isize>,
        opts: NSTextContentManagerEnumerationOptions,
        stop_after: usize,
    ) -> (Vec<(Retained<NSTextElement>, (isize, isize))>, Option<isize>) {
        let out = RefCell::new(Vec::new());
        let cm = self.cm();
        let block = RcBlock::new(|e: NonNull<NSTextElement>| -> Bool {
            // SAFETY: alive for the call.
            let e = unsafe { e.as_ref() };
            let r = e.elementRange().map(|r| rng(cm, &r)).expect("an element range");
            out.borrow_mut().push((e.retain(), r));
            Bool::new(out.borrow().len() < stop_after)
        });
        let from = from.map(|i| self.at(i));
        let ret = self.cs.enumerateTextElementsFromLocation_options_usingBlock(from.as_deref(), opts, &block);
        drop(block);
        (out.into_inner(), ret.map(|l| self.off(&l)))
    }
}

fn off(cm: &NSTextContentManager, l: &ProtocolObject<dyn NSTextLocation>) -> isize {
    cm.offsetFromLocation_toLocation(&cm.documentRange().location(), l)
}

fn rng(cm: &NSTextContentManager, r: &NSTextRange) -> (isize, isize) {
    (off(cm, &r.location()), off(cm, &r.endLocation()))
}

#[derive(Clone, Debug)]
struct Line {
    chars: (usize, usize),
    typo: NSRect,
    glyph_origin: NSPoint,
    loc0: NSPoint,
}

#[derive(Clone, Debug)]
struct Frag {
    object: Retained<NSTextLayoutFragment>,
    class: String,
    range: (isize, isize),
    state: usize,
    frame: NSRect,
    surface: NSRect,
    lines: Vec<Line>,
}

fn frag(cm: &NSTextContentManager, f: &NSTextLayoutFragment) -> Frag {
    // textLineFragments is nil on macOS before the first layout.
    let lines: Option<Retained<NSArray<objc2_app_kit::NSTextLineFragment>>> =
        unsafe { msg_send![f, textLineFragments] };
    let lines = lines
        .map(|a| {
            a.iter()
                .map(|l| {
                    let r = l.characterRange();
                    Line {
                        chars: (r.location, r.length),
                        typo: l.typographicBounds(),
                        glyph_origin: l.glyphOrigin(),
                        loc0: l.locationForCharacterAtIndex(r.location as isize),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let obj: &AnyObject = f;
    Frag {
        object: f.retain(),
        class: obj.class().name().to_str().unwrap_or("").to_owned(),
        range: rng(cm, &f.rangeInElement()),
        state: f.state().0,
        frame: f.layoutFragmentFrame(),
        surface: f.renderingSurfaceBounds(),
        lines,
    }
}

fn is_equal(a: &AnyObject, b: &AnyObject) -> bool {
    unsafe { msg_send![a, isEqual: b] }
}

fn hash(a: &AnyObject) -> usize {
    unsafe { msg_send![a, hash] }
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.01
}

fn max_y(r: NSRect) -> f64 {
    r.origin.y + r.size.height
}

// Locations and ranges.

#[test]
fn locations_and_ranges() {
    let d = Doc::new("abcdefghij", 300.0);
    let doc = d.cs.documentRange();
    assert_eq!(rng(d.cm(), &doc), (0, 10));
    assert!(!doc.isEmpty());
    // Locations move within the document only.
    let start = doc.location();
    assert!(d.cs.locationFromLocation_withOffset(&start, 10).is_some());
    assert!(d.cs.locationFromLocation_withOffset(&start, 11).is_none());
    assert!(d.cs.locationFromLocation_withOffset(&start, -1).is_none());
    let l2 = d.at(2);
    assert_eq!(d.off(&l2), 2);
    assert_eq!(l2.compare(&start), NSComparisonResult::Descending);
    assert_eq!(start.compare(&l2), NSComparisonResult::Ascending);
    assert_eq!(l2.compare(&d.at(2)), NSComparisonResult::Same);
    let l2_obj: &AnyObject = unsafe { &*(&*l2 as *const ProtocolObject<dyn NSTextLocation>).cast() };
    let again = d.at(2);
    let again_obj: &AnyObject = unsafe { &*(&*again as *const ProtocolObject<dyn NSTextLocation>).cast() };
    assert!(is_equal(l2_obj, again_obj));
    let desc: Retained<NSString> = unsafe { msg_send![l2_obj, description] };
    assert_eq!(desc.to_string(), "2");

    // A range ending before it starts is nil; one ending where it starts is
    // empty.
    assert!(NSTextRange::initWithLocation_endLocation(NSTextRange::alloc(), &d.at(3), Some(&d.at(2))).is_none());
    let empty = d.range(3, 3);
    assert!(empty.isEmpty());
    let desc: Retained<NSString> = unsafe { msg_send![&*empty, description] };
    assert_eq!(desc.to_string(), "3...3");
    let one = NSTextRange::initWithLocation(NSTextRange::alloc(), &d.at(4));
    assert_eq!(rng(d.cm(), &one), (4, 4));
    assert!(one.isEmpty());

    let a = d.range(2, 5);
    let desc: Retained<NSString> = unsafe { msg_send![&*a, description] };
    assert_eq!(desc.to_string(), "2...5");
    assert!(a.containsLocation(&d.at(2)) && a.containsLocation(&d.at(4)));
    assert!(!a.containsLocation(&d.at(5)) && !a.containsLocation(&d.at(1)));
    assert!(!empty.containsLocation(&d.at(3)), "an empty range contains nothing");
    // Touching ranges don't intersect.
    let b = d.range(5, 7);
    assert!(!a.intersectsWithTextRange(&b));
    assert!(a.textRangeByIntersectingWithTextRange(&b).is_none());
    let c = d.range(4, 7);
    assert!(a.intersectsWithTextRange(&c));
    assert_eq!(a.textRangeByIntersectingWithTextRange(&c).map(|r| rng(d.cm(), &r)), Some((4, 5)));
    assert_eq!(rng(d.cm(), &a.textRangeByFormingUnionWithTextRange(&c)), (2, 7));
    let far = d.range(8, 9);
    assert!(a.textRangeByIntersectingWithTextRange(&far).is_none());
    assert_eq!(rng(d.cm(), &a.textRangeByFormingUnionWithTextRange(&far)), (2, 9));
    // Empty ranges inside: contained where their start is, not intersecting.
    assert!(!a.intersectsWithTextRange(&empty) && a.containsRange(&empty));
    assert!(!a.containsRange(&d.range(5, 5)));
    assert!(a.containsRange(&d.range(2, 2)));
    assert!(a.containsRange(&a) && !a.containsRange(&d.range(1, 5)));
    assert!(a.isEqualToTextRange(&d.range(2, 5)) && !a.isEqualToTextRange(&c));
    let a_obj: &AnyObject = &a;
    let other = d.range(2, 5);
    assert!(is_equal(a_obj, &other));
    assert_eq!(hash(a_obj), hash(&other));
}

// Elements.

#[test]
fn paragraphs_as_elements() {
    let d = Doc::new("ab\ncd\n", 300.0);
    let none = NSTextContentManagerEnumerationOptions::None;
    let reverse = NSTextContentManagerEnumerationOptions::Reverse;
    let (all, end) = d.elements(None, none, usize::MAX);
    assert_eq!(all.iter().map(|e| e.1).collect::<Vec<_>>(), [(0, 3), (3, 6)]);
    assert_eq!(end, Some(6));
    // The same elements each time.
    let (again, _) = d.elements(None, none, usize::MAX);
    assert!(all.iter().zip(&again).all(|(a, b)| std::ptr::eq(&*a.0, &*b.0)));

    let block = RcBlock::new(|e: NonNull<NSTextElement>| -> Bool {
        let e = unsafe { e.as_ref() };
        let p = e.downcast_ref::<NSTextParagraph>().expect("a paragraph");
        let cm = e.textContentManager().expect("its content manager");
        let r = rng(&cm, &e.elementRange().expect("a range"));
        let text = p.attributedString().string().to_string();
        let content = p.paragraphContentRange().map(|c| rng(&cm, &c));
        let sep = p.paragraphSeparatorRange().map(|s| rng(&cm, &s));
        match r.0 {
            0 => assert_eq!((text.as_str(), content, sep), ("ab\n", Some((0, 2)), Some((2, 3)))),
            _ => assert_eq!((text.as_str(), content, sep), ("cd\n", Some((3, 5)), Some((5, 6)))),
        }
        assert!(e.isRepresentedElement());
        assert_eq!(e.childElements().count(), 0);
        Bool::YES
    });
    d.cs.enumerateTextElementsFromLocation_options_usingBlock(None, none, &block);

    // Forward from a location: from its element.
    assert_eq!(d.elements(Some(4), none, usize::MAX).0.iter().map(|e| e.1).collect::<Vec<_>>(), [(3, 6)]);
    assert_eq!(d.elements(Some(3), none, usize::MAX).0.iter().map(|e| e.1).collect::<Vec<_>>(), [(3, 6)]);
    let (at_end, ret) = d.elements(Some(6), none, usize::MAX);
    assert!(at_end.is_empty());
    assert_eq!(ret, Some(6));
    // In reverse: from the element before the location; from nil, nothing.
    let (r4, ret) = d.elements(Some(4), reverse, usize::MAX);
    assert_eq!(r4.iter().map(|e| e.1).collect::<Vec<_>>(), [(3, 6), (0, 3)]);
    assert_eq!(ret, Some(0));
    assert_eq!(d.elements(Some(3), reverse, usize::MAX).0.iter().map(|e| e.1).collect::<Vec<_>>(), [(0, 3)]);
    let (r_nil, ret) = d.elements(None, reverse, usize::MAX);
    assert!(r_nil.is_empty());
    assert_eq!(ret, Some(0));
    // Stopping: the end (or start, in reverse) of the element stopped on.
    assert_eq!(d.elements(None, none, 1).1, Some(3));
    assert_eq!(d.elements(Some(4), reverse, 1).1, Some(3));

    let found = d.cs.textElementsForRange(&d.cs.documentRange());
    assert_eq!(found.count(), 2);
    let found = d.cs.textElementsForRange(&d.range(3, 4));
    assert_eq!(found.iter().map(|e| e.elementRange().map(|r| rng(d.cm(), &r))).collect::<Vec<_>>(), [Some((3, 6))]);
    let first = d.cs.textElementsForRange(&d.cs.documentRange()).objectAtIndex(0);
    assert_eq!(d.cs.attributedStringForTextElement(&first).map(|a| a.string().to_string()).as_deref(), Some("ab\n"));
    assert_eq!(d.cs.attributedString().map(|a| a.string().to_string()).as_deref(), Some("ab\ncd\n"));

    // Separators: CR LF, U+2029 and a lone CR end paragraphs; U+2028 doesn't.
    let d = Doc::new("a\r\nb\u{2029}c\u{2028}d\re", 300.0);
    let cm = d.cm();
    let seen = RefCell::new(Vec::new());
    let block = RcBlock::new(|e: NonNull<NSTextElement>| -> Bool {
        let e = unsafe { e.as_ref() };
        let p = e.downcast_ref::<NSTextParagraph>().expect("a paragraph");
        seen.borrow_mut().push((
            rng(cm, &e.elementRange().expect("range")),
            p.paragraphContentRange().map(|r| rng(cm, &r)),
            p.paragraphSeparatorRange().map(|r| rng(cm, &r)),
        ));
        Bool::YES
    });
    d.cs.enumerateTextElementsFromLocation_options_usingBlock(None, none, &block);
    drop(block);
    assert_eq!(
        seen.into_inner(),
        [
            ((0, 3), Some((0, 1)), Some((1, 3))),
            ((3, 5), Some((3, 4)), Some((4, 5))),
            ((5, 9), Some((5, 8)), Some((8, 9))),
            ((9, 10), Some((9, 10)), Some((10, 10))),
        ]
    );

    // An empty text has no elements, and enumerating it returns nil.
    let d = Doc::new("", 300.0);
    let (none_found, ret) = d.elements(None, none, usize::MAX);
    assert!(none_found.is_empty() && ret.is_none());
    assert_eq!(rng(d.cm(), &d.cs.documentRange()), (0, 0));

    // A paragraph of its own has no range or content manager.
    let p = NSTextParagraph::initWithAttributedString(NSTextParagraph::alloc(), Some(&attributed("x\n", None)));
    assert!(p.elementRange().is_none() && p.textContentManager().is_none());
    assert!(p.paragraphContentRange().is_none() && p.paragraphSeparatorRange().is_none());
    let nil: Option<Retained<NSAttributedString>> = unsafe {
        msg_send![&*NSTextParagraph::initWithAttributedString(NSTextParagraph::alloc(), None), attributedString]
    };
    assert!(nil.is_none());

    // A new content storage has a storage of its own.
    assert!(NSTextContentStorage::new().textStorage().is_some());
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextKit2TestSubstitute"]
    #[ivars = RefCell<Vec<(usize, usize)>>]
    struct Substitute;

    unsafe impl NSObjectProtocol for Substitute {}
    unsafe impl NSTextContentManagerDelegate for Substitute {}
    unsafe impl NSTextContentStorageDelegate for Substitute {
        #[unsafe(method_id(textContentStorage:textParagraphWithRange:))]
        fn paragraph(&self, _cs: &NSTextContentStorage, range: NSRange) -> Option<Retained<NSTextParagraph>> {
            self.ivars().borrow_mut().push((range.location, range.length));
            if range.location == 0 {
                let style = NSMutableParagraphStyle::new();
                style.setParagraphSpacing(10.0);
                style.setParagraphSpacingBefore(4.0);
                let text = attributed("o\nw\n", Some(&style));
                Some(NSTextParagraph::initWithAttributedString(NSTextParagraph::alloc(), Some(&text)))
            } else {
                None
            }
        }
    }
);

fn substitute() -> Retained<Substitute> {
    let this = Substitute::alloc().set_ivars(RefCell::new(Vec::new()));
    unsafe { msg_send![super(this), init] }
}

/// A delegate's paragraph stands for the storage's: asked once for each,
/// with the paragraph's range; it gets the range and the content manager,
/// its separator is its own text's, and each of its own paragraphs lays out
/// as a paragraph, spaced as paragraphs are.
#[test]
fn delegate_paragraphs() {
    let d = Doc::new("one\ntwo\nthree\nfour", 300.0);
    let s = substitute();
    unsafe { d.cs.setDelegate(Some(ProtocolObject::from_ref(&*s))) };
    let (all, _) = d.elements(None, NSTextContentManagerEnumerationOptions::None, usize::MAX);
    assert_eq!(all.iter().map(|e| e.1).collect::<Vec<_>>(), [(0, 4), (4, 8), (8, 14), (14, 18)]);
    assert_eq!(*s.ivars().borrow(), [(0, 4), (4, 4), (8, 6), (14, 4)]);
    s.ivars().borrow_mut().clear();
    d.elements(None, NSTextContentManagerEnumerationOptions::None, usize::MAX);
    assert!(s.ivars().borrow().is_empty(), "asked once");
    let first = d.cs.textElementsForRange(&d.range(0, 1)).objectAtIndex(0);
    let p = first.downcast_ref::<NSTextParagraph>().expect("a paragraph");
    assert_eq!(p.attributedString().string().to_string(), "o\nw\n");
    assert!(p.textContentManager().is_some_and(|m| std::ptr::eq(&*m, d.cm())));
    assert_eq!(p.paragraphContentRange().map(|r| rng(d.cm(), &r)), Some((0, 3)));
    assert_eq!(p.paragraphSeparatorRange().map(|r| rng(d.cm(), &r)), Some((3, 4)));

    let laid = d.laid();
    assert_eq!(laid.len(), 4);
    let f = &laid[0];
    assert_eq!(f.lines.len(), 2, "each of its paragraphs has a line");
    assert_eq!(f.lines.iter().map(|l| l.chars).collect::<Vec<_>>(), [(0, 2), (2, 2)]);
    let (l0, l1) = (&f.lines[0], &f.lines[1]);
    assert!(near(l0.typo.origin.y, 0.0));
    // The spacing after the first and before the second.
    assert!(near(l1.typo.origin.y, max_y(l0.typo) + 10.0 + 4.0), "{f:?}");
    assert!(near(f.frame.size.height, max_y(l1.typo) + 10.0), "{f:?}");
    assert!(near(laid[1].frame.origin.y, max_y(f.frame)));
}

// Edits.

#[test]
fn edits_keep_what_they_dont_touch() {
    let d = Doc::new("aa\nbb\ncc\ndd", 300.0);
    let ts = d.cs.textStorage().expect("a storage");
    let observer: Option<Retained<AnyObject>> = unsafe { msg_send![&*ts, textStorageObserver] };
    assert!(observer.is_some_and(|o| std::ptr::eq(&*o, &*d.cs as &AnyObject)));
    assert_eq!(ts.layoutManagers().count(), 0);
    let (before, _) = d.elements(None, NSTextContentManagerEnumerationOptions::None, usize::MAX);
    let frags_before = d.laid();
    assert!(frags_before.iter().all(|f| f.state == 3));
    ts.replaceCharactersInRange_withString(NSRange::new(4, 0), &NSString::from_str("X"));
    let (after, _) = d.elements(None, NSTextContentManagerEnumerationOptions::None, usize::MAX);
    assert_eq!(after.iter().map(|e| e.1).collect::<Vec<_>>(), [(0, 3), (3, 7), (7, 10), (10, 12)]);
    let same: Vec<bool> = after.iter().zip(&before).map(|(a, b)| std::ptr::eq(&*a.0, &*b.0)).collect();
    assert_eq!(same, [true, false, true, true]);
    let (frags, _) = d.fragments(NSTextLayoutFragmentEnumerationOptions::None, None);
    let same: Vec<bool> = frags.iter().zip(&frags_before).map(|(a, b)| std::ptr::eq(&*a.object, &*b.object)).collect();
    assert_eq!(same, [true, false, true, true]);
    assert_eq!(frags.iter().map(|f| f.state).collect::<Vec<_>>(), [3, 0, 3, 3]);
    assert_eq!(frags.iter().map(|f| f.range).collect::<Vec<_>>(), [(0, 3), (3, 7), (7, 10), (10, 12)]);
    // An element's range moves with the text.
    assert_eq!(d.cs.documentRange().endLocation().compare(&d.at(12)), NSComparisonResult::Same);

    // Editing in a transaction.
    assert!(!d.cs.hasEditingTransaction());
    let inside = RefCell::new(false);
    let block = RcBlock::new(|| {
        *inside.borrow_mut() = d.cs.hasEditingTransaction();
    });
    d.cs.performEditingTransactionUsingBlock(&block);
    assert!(*inside.borrow());
    assert!(!d.cs.hasEditingTransaction());
}

// Layout.

#[test]
fn fragments_and_lines() {
    let text = "Hello world\nSecond line that is long enough to wrap around the container more than once\n";
    let d = Doc::new(text, 150.0);
    let padding = d.container.lineFragmentPadding();
    assert!(near(padding, 5.0));
    // Before layout: fragments in state 0 with no frame.
    let (unlaid, ret) = d.fragments(NSTextLayoutFragmentEnumerationOptions::None, None);
    assert_eq!(unlaid.iter().map(|f| (f.range, f.state)).collect::<Vec<_>>(), [((0, 12), 0), ((12, 88), 0)]);
    assert!(unlaid.iter().all(|f| f.frame.size == NSSize::ZERO));
    assert_eq!(ret, Some(88));
    assert_eq!(d.tlm.usageBoundsForTextContainer(), NSRect::ZERO);

    let (laid, ret) = d.fragments(NSTextLayoutFragmentEnumerationOptions::EnsuresLayout, None);
    assert_eq!(ret, Some(88));
    assert!(laid.iter().all(|f| f.state == 3 && f.class == "NSTextLayoutFragment"));
    let (a, b) = (&laid[0], &laid[1]);
    // Frames start at the padding and stack.
    assert!(near(a.frame.origin.x, padding) && near(a.frame.origin.y, 0.0));
    assert!(near(b.frame.origin.y, max_y(a.frame)));
    assert_eq!(a.lines.len(), 1);
    assert!(b.lines.len() >= 3, "wraps: {b:?}");
    // Lines: character ranges in the element's text, typographic bounds in
    // the frame, stacked, the frame as wide as the widest.
    let widest = b.lines.iter().map(|l| l.typo.origin.x + l.typo.size.width).fold(0.0, f64::max);
    assert!(near(b.frame.size.width, widest));
    let mut y = 0.0;
    let mut next = 0;
    for l in &b.lines {
        assert!(near(l.typo.origin.y, y), "{b:?}");
        assert!(near(l.typo.origin.x, 0.0));
        assert_eq!(l.chars.0, next);
        next = l.chars.0 + l.chars.1;
        y += l.typo.size.height;
        assert!(l.glyph_origin.y > 0.0 && l.glyph_origin.y < l.typo.size.height);
        assert!(near(l.glyph_origin.x, 0.0));
        assert!(near(l.loc0.x, 0.0) && near(l.loc0.y, l.glyph_origin.y));
    }
    assert!(near(b.frame.size.height, y));
    // The text ends in a separator: its last fragment ends in an empty line.
    let last = b.lines.last().expect("lines");
    assert_eq!(last.chars, (76, 0));
    assert!(near(last.typo.size.width, 0.0));
    // The rendering surface takes in the lines.
    for f in &laid {
        for l in &f.lines {
            assert!(f.surface.origin.x <= l.typo.origin.x && f.surface.origin.y <= l.typo.origin.y);
            assert!(max_y(f.surface) >= max_y(l.typo));
        }
    }
    let usage = d.tlm.usageBoundsForTextContainer();
    assert!(near(usage.origin.x, padding) && near(usage.origin.y, 0.0));
    assert!(near(max_y(usage), max_y(b.frame)));
    assert!(near(usage.size.width, a.frame.size.width.max(b.frame.size.width)));

    // In reverse: from the end; from a location, from the fragment before it.
    let rev = NSTextLayoutFragmentEnumerationOptions::Reverse;
    let (r, ret) = d.fragments(rev, None);
    assert_eq!(r.iter().map(|f| f.range).collect::<Vec<_>>(), [(12, 88), (0, 12)]);
    assert_eq!(ret, Some(0));
    assert_eq!(d.fragments(rev, Some(14)).0.iter().map(|f| f.range).collect::<Vec<_>>(), [(12, 88), (0, 12)]);
    assert_eq!(d.fragments(rev, Some(12)).0.iter().map(|f| f.range).collect::<Vec<_>>(), [(0, 12)]);
    assert_eq!(d.fragments(NSTextLayoutFragmentEnumerationOptions::None, Some(14)).0.len(), 1);

    // Finding fragments.
    let at =
        |y: f64| d.tlm.textLayoutFragmentForPosition(NSPoint::new(5.0, y)).map(|f| rng(d.cm(), &f.rangeInElement()));
    assert_eq!(at(1.0), Some((0, 12)));
    assert_eq!(at(max_y(a.frame) + 1.0), Some((12, 88)));
    assert_eq!(at(-3.0), Some((0, 12)));
    assert_eq!(at(max_y(b.frame) + 100.0), None);
    for (i, want) in [(0, (0, 12)), (11, (0, 12)), (12, (12, 88)), (20, (12, 88))] {
        let f = d.tlm.textLayoutFragmentForLocation(&d.at(i)).expect("a fragment");
        assert_eq!(rng(d.cm(), &f.rangeInElement()), want);
        assert!(f.textElement().is_some_and(|e| e.elementRange().is_some_and(|r| rng(d.cm(), &r) == want)));
        assert!(f.textLayoutManager().is_some_and(|m| std::ptr::eq(&*m, &*d.tlm)));
    }
    // The layout manager answers as a selection data source too.
    assert_eq!(rng(d.cm(), &d.tlm.documentRange()), (0, 88));
    assert_eq!(d.tlm.offsetFromLocation_toLocation(&d.at(0), &d.at(5)), 5);
}

#[test]
fn spacing() {
    let style = NSMutableParagraphStyle::new();
    style.setParagraphSpacing(10.0);
    style.setParagraphSpacingBefore(4.0);
    style.setLineSpacing(3.0);
    // A paragraph that wraps, one that doesn't, and the text's end.
    let d = Doc::styled("aaa bbb ccc ddd eee fff ggg hhh\nb\n", 60.0, Some(&style));
    let laid = d.laid();
    assert_eq!(laid.len(), 2);
    let (a, b) = (&laid[0], &laid[1]);
    assert!(a.lines.len() >= 2);
    // Line spacing between lines, the spacing after the paragraph after its
    // last line, none before the text's first.
    assert!(near(a.lines[0].typo.origin.y, 0.0));
    for w in a.lines.windows(2) {
        assert!(near(w[1].typo.origin.y, max_y(w[0].typo) + 3.0), "{a:?}");
    }
    assert!(near(a.frame.size.height, max_y(a.lines.last().expect("lines").typo) + 10.0), "{a:?}");
    // The next paragraph: its spacing before and line spacing above it.
    assert!(near(b.frame.origin.y, max_y(a.frame)));
    assert!(near(b.lines[0].typo.origin.y, 4.0 + 3.0), "{b:?}");
    // The empty line after the final separator is spaced as a paragraph,
    // and nothing follows the text's last line.
    assert_eq!(b.lines.len(), 2);
    assert!(near(b.lines[1].typo.origin.y, max_y(b.lines[0].typo) + 10.0 + 4.0 + 3.0), "{b:?}");
    assert!(near(b.frame.size.height, max_y(b.lines[1].typo)), "{b:?}");

    // Indents and alignment move the frame, which starts at the leftmost
    // line.
    let first = NSMutableParagraphStyle::new();
    first.setFirstLineHeadIndent(7.0);
    let d = Doc::styled("indented\nsecond", 300.0, Some(&first));
    let laid = d.laid();
    assert!(near(laid[0].frame.origin.x, 5.0 + 7.0), "{laid:?}");
    assert!(near(laid[0].lines[0].typo.origin.x, 0.0));
    let center = NSMutableParagraphStyle::new();
    center.setAlignment(objc2_app_kit::NSTextAlignment::Center);
    let d = Doc::styled("mid", 200.0, Some(&center));
    let f = &d.laid()[0];
    assert!(near(f.frame.origin.x, 5.0 + (190.0 - f.frame.size.width) / 2.0), "{f:?}");

    // Without padding the frame starts at the edge; a wider container
    // throws the layout away (state 0, no frame, no lines).
    d.container.setLineFragmentPadding(0.0);
    let f = &d.laid()[0];
    assert!(near(f.frame.origin.x, (200.0 - f.frame.size.width) / 2.0), "{f:?}");
    d.container.setSize(NSSize::new(300.0, 1.0e7));
    let (after, _) = d.fragments(NSTextLayoutFragmentEnumerationOptions::None, None);
    assert!(after.iter().all(|f| f.state == 0 && f.frame == NSRect::ZERO && f.lines.is_empty()), "{after:?}");
}

#[test]
fn empty_and_extra_line() {
    let extra = NSTextLayoutFragmentEnumerationOptions::EnsuresExtraLineFragment
        | NSTextLayoutFragmentEnumerationOptions::EnsuresLayout;
    let d = Doc::new("", 300.0);
    let (none, ret) = d.fragments(NSTextLayoutFragmentEnumerationOptions::EnsuresLayout, None);
    assert!(none.is_empty() && ret.is_none());
    assert_eq!(d.tlm.usageBoundsForTextContainer(), NSRect::ZERO);
    let (one, ret) = d.fragments(extra, None);
    assert!(ret.is_none());
    assert_eq!(one.len(), 1);
    let f = &one[0];
    assert_eq!((f.range, f.state, f.lines.len()), ((0, 0), 3, 1));
    assert!(near(f.frame.origin.x, 5.0) && near(f.frame.size.width, 0.0) && f.frame.size.height > 0.0);
    assert!(near(f.frame.size.height, f.lines[0].typo.size.height));

    // A text of a separator: one fragment, its line and the empty one.
    let d = Doc::new("\n", 300.0);
    let laid = d.laid();
    assert_eq!(laid.len(), 1);
    assert_eq!(laid[0].lines.iter().map(|l| l.chars).collect::<Vec<_>>(), [(0, 1), (1, 0)]);
    // The option adds nothing where the text doesn't end in a separator.
    let d = Doc::new("aaa\nb", 300.0);
    assert_eq!(d.fragments(extra, None).0.iter().map(|f| f.lines.len()).collect::<Vec<_>>(), [1, 1]);
}

#[test]
fn segments() {
    let d = Doc::new("Hello world\nSecond line\n", 150.0);
    let cm = d.cm();
    let collect = |r: &NSTextRange, kind: NSTextLayoutManagerSegmentType| {
        let out = RefCell::new(Vec::new());
        let block = RcBlock::new(
            |range: *mut NSTextRange, frame: NSRect, baseline: f64, _c: NonNull<NSTextContainer>| -> Bool {
                let r = unsafe { range.as_ref() }.map(|r| rng(cm, r));
                out.borrow_mut().push((r, frame, baseline));
                Bool::YES
            },
        );
        d.tlm.enumerateTextSegmentsInRange_type_options_usingBlock(
            r,
            kind,
            NSTextLayoutManagerSegmentOptions::None,
            &block,
        );
        drop(block);
        out.into_inner()
    };
    let laid = d.laid();
    let line_h = laid[0].lines[0].typo.size.height;
    let caret = collect(
        &NSTextRange::initWithLocation(NSTextRange::alloc(), &d.at(3)),
        NSTextLayoutManagerSegmentType::Standard,
    );
    assert_eq!(caret.len(), 1);
    let (r, frame, baseline) = caret[0];
    assert_eq!(r, Some((3, 3)));
    assert!(near(frame.size.width, 0.0) && near(frame.size.height, line_h) && near(frame.origin.y, 0.0));
    assert!(frame.origin.x > 5.0 && near(baseline, laid[0].lines[0].glyph_origin.y));
    let span = d.range(3, 18);
    let standard = collect(&span, NSTextLayoutManagerSegmentType::Standard);
    assert_eq!(standard.iter().map(|s| s.0).collect::<Vec<_>>(), [Some((3, 12)), Some((12, 18))]);
    assert!(near(standard[1].1.origin.y, max_y(laid[0].frame)) && near(standard[1].1.origin.x, 5.0));
    // A selection taking in a separator reaches the container's far edge.
    let selection = collect(&span, NSTextLayoutManagerSegmentType::Selection);
    assert!(near(selection[0].1.origin.x + selection[0].1.size.width, 150.0 - 5.0), "{selection:?}");
    assert!(standard[0].1.origin.x + standard[0].1.size.width < 150.0 - 5.0);
}

#[test]
fn line_fragment_queries() {
    let d = Doc::new("MMMM MMMM MMMM MMMM MMMM\nx", 80.0);
    d.tlm.ensureLayoutForRange(&d.cs.documentRange());
    let f = d.tlm.textLayoutFragmentForLocation(&d.at(0)).expect("a fragment");
    let lines = f.textLineFragments();
    assert!(lines.count() >= 2);
    let l0 = lines.objectAtIndex(0);
    let l1 = lines.objectAtIndex(1);
    let h = l0.typographicBounds().size.height;
    let exact =
        |y: f64| f.textLineFragmentForVerticalOffset_requiresExactMatch(y, true).map(|l| l.characterRange().location);
    assert_eq!(exact(1.0), Some(0));
    assert_eq!(exact(h + 1.0), Some(l1.characterRange().location));
    assert_eq!(exact(1000.0), None);
    assert!(f.textLineFragmentForVerticalOffset_requiresExactMatch(1000.0, false).is_none());
    let for_loc = |i: isize| {
        f.textLineFragmentForTextLocation_isUpstreamAffinity(&d.at(i), false).map(|l| l.characterRange().location)
    };
    assert_eq!(for_loc(0), Some(0));
    assert_eq!(for_loc(l1.characterRange().location as isize + 1), Some(l1.characterRange().location));
    assert_eq!(for_loc(25), None, "outside the fragment");
    // Points inside the first character.
    let w = l0.locationForCharacterAtIndex(1).x;
    assert!(w > 0.0);
    assert_eq!(l0.characterIndexForPoint(NSPoint::new(w * 0.25, h / 2.0)), 0);
    let frac = l0.fractionOfDistanceThroughGlyphForPoint(NSPoint::new(w * 0.25, h / 2.0));
    assert!((frac - 0.25).abs() < 0.05, "{frac}");
    assert_eq!(l0.attributedString().string().to_string(), "MMMM MMMM MMMM MMMM MMMM\n");
}

define_class!(
    #[unsafe(super(NSTextLayoutFragment))]
    #[name = "TextKit2TestZeroFragment"]
    struct ZeroFragment;

    impl ZeroFragment {
        #[unsafe(method(layoutFragmentFrame))]
        fn frame(&self) -> NSRect {
            let mut f: NSRect = unsafe { msg_send![super(self), layoutFragmentFrame] };
            f.size.height = 0.0;
            f
        }
    }
);

define_class!(
    #[unsafe(super(NSTextLayoutFragment))]
    #[name = "TextKit2TestTallFragment"]
    struct TallFragment;

    impl TallFragment {
        #[unsafe(method(layoutFragmentFrame))]
        fn frame(&self) -> NSRect {
            let mut f: NSRect = unsafe { msg_send![super(self), layoutFragmentFrame] };
            f.size.height += 100.0;
            f
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextKit2TestFragments"]
    #[ivars = RefCell<Vec<isize>>]
    struct Fragments;

    unsafe impl NSObjectProtocol for Fragments {}
    unsafe impl NSTextLayoutManagerDelegate for Fragments {
        #[unsafe(method_id(textLayoutManager:textLayoutFragmentForLocation:inTextElement:))]
        fn fragment(
            &self,
            tlm: &NSTextLayoutManager,
            location: &ProtocolObject<dyn NSTextLocation>,
            element: &NSTextElement,
        ) -> Retained<NSTextLayoutFragment> {
            let cm = tlm.textContentManager().expect("a content manager");
            let o = off(&cm, location);
            self.ivars().borrow_mut().push(o);
            let range = element.elementRange();
            match o {
                0 => {
                    let this = ZeroFragment::alloc().set_ivars(());
                    let f: Retained<ZeroFragment> =
                        unsafe { msg_send![super(this), initWithTextElement: element, range: range.as_deref()] };
                    Retained::into_super(f)
                }
                6 => {
                    let this = TallFragment::alloc().set_ivars(());
                    let f: Retained<TallFragment> =
                        unsafe { msg_send![super(this), initWithTextElement: element, range: range.as_deref()] };
                    Retained::into_super(f)
                }
                _ => {
                    let this = NSTextLayoutFragment::alloc();
                    NSTextLayoutFragment::initWithTextElement_range(this, element, range.as_deref())
                }
            }
        }
    }
);

/// The delegate's fragments are laid out, and the frames they give place
/// what follows them: none tall moves nothing down.
#[test]
fn custom_fragments() {
    let d = Doc::new("first\nsecond\nthird\nfourth", 300.0);
    let delegate = Fragments::alloc().set_ivars(RefCell::new(Vec::new()));
    let delegate: Retained<Fragments> = unsafe { msg_send![super(delegate), init] };
    d.tlm.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let laid = d.laid();
    assert_eq!(*delegate.ivars().borrow(), [0, 6, 13, 19]);
    assert_eq!(
        laid.iter().map(|f| f.class.as_str()).collect::<Vec<_>>(),
        ["TextKit2TestZeroFragment", "TextKit2TestTallFragment", "NSTextLayoutFragment", "NSTextLayoutFragment"]
    );
    assert!(laid.iter().all(|f| f.state == 3));
    let line_h = laid[2].frame.size.height;
    assert!(near(laid[0].frame.size.height, 0.0));
    assert!(near(laid[1].frame.origin.y, 0.0), "after a fragment of no height");
    assert!(near(laid[1].frame.size.height, line_h + 100.0));
    assert!(near(laid[2].frame.origin.y, max_y(laid[1].frame)));
    assert!(near(laid[3].frame.origin.y, max_y(laid[2].frame)));
    assert!(near(max_y(d.tlm.usageBoundsForTextContainer()), max_y(laid[3].frame)));
}

#[test]
fn invalidation() {
    let d = Doc::new("one\ntwo\nthree", 300.0);
    d.tlm.ensureLayoutForRange(&d.cs.documentRange());
    let f0 = d.tlm.textLayoutFragmentForLocation(&d.at(0)).expect("a fragment");
    let frame = f0.layoutFragmentFrame();
    d.tlm.invalidateLayoutForRange(&f0.rangeInElement());
    assert_eq!(f0.state().0, 0);
    assert_eq!(f0.layoutFragmentFrame(), frame, "the frame stays");
    let again = d.tlm.textLayoutFragmentForLocation(&d.at(0)).expect("a fragment");
    assert!(std::ptr::eq(&*f0, &*again), "the same fragment");
    d.tlm.ensureLayoutForRange(&d.range(0, 1));
    assert_eq!(f0.state().0, 3);
}

#[test]
fn layout_manager_setup() {
    let tlm = NSTextLayoutManager::new();
    assert!(tlm.textContainer().is_none() && tlm.textContentManager().is_none());
    assert!(tlm.usesFontLeading());
    // No viewport controller until there is a container.
    let none: Option<Retained<NSTextViewportLayoutController>> =
        unsafe { msg_send![&*tlm, textViewportLayoutController] };
    assert!(none.is_none());
    let c = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(100.0, 100.0));
    assert!(c.textLayoutManager().is_none());
    tlm.setTextContainer(Some(&c));
    assert!(c.textLayoutManager().is_some_and(|m| std::ptr::eq(&*m, &*tlm)));
    let vp = tlm.textViewportLayoutController();
    assert!(vp.textLayoutManager().is_some_and(|m| std::ptr::eq(&*m, &*tlm)));
    assert!(vp.delegate().is_none());
    let cs = NSTextContentStorage::new();
    cs.addTextLayoutManager(&tlm);
    assert!(tlm.textContentManager().is_some_and(|m| std::ptr::eq(&*m, &*cs as &NSTextContentManager)));
    assert_eq!(cs.textLayoutManagers().count(), 1);
    // Adding a layout manager doesn't make it the primary one.
    assert!(cs.primaryTextLayoutManager().is_none());
    cs.setPrimaryTextLayoutManager(Some(&tlm));
    assert!(cs.primaryTextLayoutManager().is_some_and(|m| std::ptr::eq(&*m, &*tlm)));
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextKit2TestViewport"]
    #[ivars = RefCell<Vec<String>>]
    struct Viewport;

    unsafe impl NSObjectProtocol for Viewport {}
    unsafe impl NSTextViewportLayoutControllerDelegate for Viewport {
        #[unsafe(method(viewportBoundsForTextViewportLayoutController:))]
        fn bounds(&self, _c: &NSTextViewportLayoutController) -> NSRect {
            self.ivars().borrow_mut().push("bounds".into());
            NSRect::new(NSPoint::new(0.0, 20.0), NSSize::new(200.0, 30.0))
        }

        #[unsafe(method(textViewportLayoutController:configureRenderingSurfaceForTextLayoutFragment:))]
        fn configure(&self, c: &NSTextViewportLayoutController, f: &NSTextLayoutFragment) {
            let cm = c.textLayoutManager().and_then(|m| m.textContentManager()).expect("a content manager");
            let r = rng(&cm, &f.rangeInElement());
            self.ivars().borrow_mut().push(format!("configure {} {} state {}", r.0, r.1, f.state().0));
        }

        #[unsafe(method(textViewportLayoutControllerWillLayout:))]
        fn will(&self, _c: &NSTextViewportLayoutController) {
            self.ivars().borrow_mut().push("will".into());
        }

        #[unsafe(method(textViewportLayoutControllerDidLayout:))]
        fn did(&self, _c: &NSTextViewportLayoutController) {
            self.ivars().borrow_mut().push("did".into());
        }
    }
);

/// Laying out the viewport: the delegate's calls in order, the fragments
/// the bounds meet configured (laid out), the viewport range theirs.
#[test]
fn viewport_layout() {
    let d = Doc::new("first\nsecond\nthird\nfourth", 300.0);
    let delegate = Fragments::alloc().set_ivars(RefCell::new(Vec::new()));
    let delegate: Retained<Fragments> = unsafe { msg_send![super(delegate), init] };
    d.tlm.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let v = Viewport::alloc().set_ivars(RefCell::new(Vec::new()));
    let v: Retained<Viewport> = unsafe { msg_send![super(v), init] };
    let vp = d.tlm.textViewportLayoutController();
    vp.setDelegate(Some(ProtocolObject::from_ref(&*v)));
    assert_eq!(vp.viewportBounds(), NSRect::ZERO);
    assert!(vp.viewportRange().is_none());
    vp.layoutViewport();
    // The tall fragment (0 to its natural height and 100 more) holds the
    // bounds, 20 to 50.
    assert_eq!(*v.ivars().borrow(), ["will", "bounds", "configure 6 13 state 3", "did"]);
    assert_eq!(vp.viewportBounds(), NSRect::new(NSPoint::new(0.0, 20.0), NSSize::new(200.0, 30.0)));
    assert_eq!(vp.viewportRange().map(|r| rng(d.cm(), &r)), Some((6, 13)));
    v.ivars().borrow_mut().clear();
    vp.adjustViewportByVerticalOffset(10.0);
    assert!(v.ivars().borrow().is_empty());
    assert_eq!(vp.viewportBounds().origin.y, 30.0);
}

/// A long text: laying out the viewport lays out what it shows, and the
/// rest is estimated; a fragment far down is laid out where the estimates
/// put it.
#[test]
fn long_text_lays_out_the_viewport() {
    let text: String = (0..2000).map(|i| format!("line number {i}\n")).collect();
    let d = Doc::new(&text, 300.0);
    let v = Viewport::alloc().set_ivars(RefCell::new(Vec::new()));
    let v: Retained<Viewport> = unsafe { msg_send![super(v), init] };
    let vp = d.tlm.textViewportLayoutController();
    vp.setDelegate(Some(ProtocolObject::from_ref(&*v)));
    vp.layoutViewport();
    let configured = v.ivars().borrow().iter().filter(|s| s.starts_with("configure")).count();
    assert!((1..10).contains(&configured), "{configured}");
    let (all, _) = d.fragments(NSTextLayoutFragmentEnumerationOptions::None, None);
    assert_eq!(all.len(), 2000);
    let laid = all.iter().filter(|f| f.state == 3).count();
    assert!(laid < 50, "{laid} laid out");
    // Without a view asking for the rest's estimate, the usage bounds are
    // what is laid out.
    let usage = d.tlm.usageBoundsForTextContainer();
    let bottom = all.iter().filter(|f| f.state == 3).map(|f| max_y(f.frame)).fold(0.0, f64::max);
    assert!(near(max_y(usage), bottom), "{usage:?} {bottom}");
    let far = d.tlm.textLayoutFragmentForLocation(&d.at(20000)).expect("a fragment");
    assert_eq!(far.state().0, 0);
    d.tlm.ensureLayoutForRange(&far.rangeInElement());
    assert_eq!(far.state().0, 3);
    assert!(far.layoutFragmentFrame().origin.y > 1000.0);
}

#[test]
fn selections() {
    let d = Doc::new("hello world\nsecond", 300.0);
    d.tlm.ensureLayoutForRange(&d.cs.documentRange());
    let r = d.range(2, 5);
    let s = NSTextSelection::initWithRange_affinity_granularity(
        NSTextSelection::alloc(),
        &r,
        NSTextSelectionAffinity::Upstream,
        NSTextSelectionGranularity::Word,
    );
    assert_eq!(s.textRanges().iter().map(|r| rng(d.cm(), &r)).collect::<Vec<_>>(), [(2, 5)]);
    assert_eq!((s.affinity(), s.granularity()), (NSTextSelectionAffinity::Upstream, NSTextSelectionGranularity::Word));
    assert!(!s.isTransient());
    let caret = NSTextSelection::initWithLocation_affinity(
        NSTextSelection::alloc(),
        &d.at(4),
        NSTextSelectionAffinity::Downstream,
    );
    assert_eq!(caret.textRanges().iter().map(|r| rng(d.cm(), &r)).collect::<Vec<_>>(), [(4, 4)]);
    assert_eq!(caret.granularity(), NSTextSelectionGranularity::Character);

    let nav = d.tlm.textSelectionNavigation();
    let source = nav.textSelectionDataSource().expect("a data source");
    assert!(std::ptr::eq(&*source as *const _ as *const AnyObject, &*d.tlm as *const _ as *const AnyObject));
    // A click at the start of the first line's third character: a caret
    // there, downstream, by character.
    let lines = d.tlm.textLayoutFragmentForLocation(&d.at(0)).expect("a fragment").textLineFragments();
    let l0 = lines.objectAtIndex(0);
    let x = 5.0 + l0.locationForCharacterAtIndex(2).x + 0.5;
    let y = l0.typographicBounds().size.height / 2.0;
    let found = nav.textSelectionsInteractingAtPoint_inContainerAtLocation_anchors_modifiers_selecting_bounds(
        NSPoint::new(x, y),
        &d.cs.documentRange().location(),
        &NSArray::new(),
        NSTextSelectionNavigationModifier(0),
        true,
        NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 1000.0)),
    );
    assert_eq!(found.count(), 1);
    let sel = found.objectAtIndex(0);
    assert_eq!(sel.textRanges().iter().map(|r| rng(d.cm(), &r)).collect::<Vec<_>>(), [(2, 2)]);
    assert_eq!(sel.affinity(), NSTextSelectionAffinity::Downstream);
    assert_eq!(sel.granularity(), NSTextSelectionGranularity::Character);
}

define_class!(
    /// A grouped element: its content range is its whole range, its
    /// separator its trailing line break (AppKit's own paragraph measures
    /// neither for an element its content storage didn't make).
    #[unsafe(super(NSTextParagraph))]
    #[name = "TextKit2TestGroupParagraph"]
    #[ivars = (Retained<NSTextRange>, Retained<NSTextRange>)]
    struct GroupParagraph;

    impl GroupParagraph {
        #[unsafe(method_id(paragraphContentRange))]
        fn content(&self) -> Option<Retained<NSTextRange>> {
            Some(self.ivars().0.clone())
        }

        #[unsafe(method_id(paragraphSeparatorRange))]
        fn separator(&self) -> Option<Retained<NSTextRange>> {
            Some(self.ivars().1.clone())
        }
    }
);

define_class!(
    /// A content storage presenting two of its storage's paragraphs as one
    /// element, as a program grouping lines into blocks does.
    #[unsafe(super(NSTextContentStorage))]
    #[name = "TextKit2TestGrouping"]
    struct Grouping;

    impl Grouping {
        #[unsafe(method_id(enumerateTextElementsFromLocation:options:usingBlock:))]
        fn enumerate(
            &self,
            from: Option<&ProtocolObject<dyn NSTextLocation>>,
            options: NSTextContentManagerEnumerationOptions,
            block: &block2::DynBlock<dyn Fn(NonNull<NSTextElement>) -> Bool + '_>,
        ) -> Option<Retained<ProtocolObject<dyn NSTextLocation>>> {
            grouped(self, from, options, block)
        }
    }
);

/// The pairs of paragraphs of `cs`'s text, as ranges.
fn pairs(cs: &NSTextContentStorage) -> Vec<(usize, usize)> {
    let text = cs.textStorage().map(|s| s.string().to_string()).unwrap_or_default();
    let mut paras = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            paras.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < text.len() {
        paras.push((start, text.len()));
    }
    paras.chunks(2).map(|c| (c[0].0, c[c.len() - 1].1)).collect()
}

fn grouped(
    cs: &Grouping,
    from: Option<&ProtocolObject<dyn NSTextLocation>>,
    options: NSTextContentManagerEnumerationOptions,
    block: &block2::DynBlock<dyn Fn(NonNull<NSTextElement>) -> Bool + '_>,
) -> Option<Retained<ProtocolObject<dyn NSTextLocation>>> {
    let storage: &NSTextContentStorage = cs;
    let start = storage.documentRange().location();
    let at = from.map_or(0, |l| storage.offsetFromLocation_toLocation(&start, l) as usize);
    let text = storage.textStorage()?;
    let mut end = at;
    for (a, b) in pairs(storage) {
        if (options.contains(NSTextContentManagerEnumerationOptions::Reverse) && a >= at)
            || (!options.contains(NSTextContentManagerEnumerationOptions::Reverse) && b <= at)
        {
            continue;
        }
        let sub = text.attributedSubstringFromRange(NSRange::new(a, b - a));
        let sep = usize::from(sub.string().to_string().ends_with('\n'));
        let at_ = |i: usize| storage.locationFromLocation_withOffset(&start, i as isize);
        let (la, lb, ls) = (at_(a)?, at_(b)?, at_(b - sep)?);
        let range = NSTextRange::initWithLocation_endLocation(NSTextRange::alloc(), &la, Some(&lb))?;
        let separator = NSTextRange::initWithLocation_endLocation(NSTextRange::alloc(), &ls, Some(&lb))?;
        let this = GroupParagraph::alloc().set_ivars((range.clone(), separator));
        let p: Retained<GroupParagraph> = unsafe { msg_send![super(this), initWithAttributedString: &*sub] };
        p.setTextContentManager(Some(storage));
        p.setElementRange(Some(&range));
        end = b;
        let element: &NSTextElement = &p;
        if !block.call((NonNull::from(element),)).as_bool() {
            break;
        }
    }
    storage.locationFromLocation_withOffset(&start, end as isize)
}

/// A content storage subclass's own elements are what is laid out: two
/// paragraphs each here, stacked as paragraphs in one fragment.
#[test]
fn a_subclass_groups_paragraphs() {
    let this = Grouping::alloc().set_ivars(());
    let cs: Retained<Grouping> = unsafe { msg_send![super(this), init] };
    let tlm = NSTextLayoutManager::new();
    let container = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(300.0, 1.0e7));
    tlm.setTextContainer(Some(&container));
    let storage: &NSTextContentStorage = &cs;
    storage.addTextLayoutManager(&tlm);
    storage.setTextStorage(Some(&storage_with("one\ntwo\nthree\nfour\nfive", None)));
    let cm: &NSTextContentManager = storage;
    let out = RefCell::new(Vec::new());
    let block = RcBlock::new(|f: NonNull<NSTextLayoutFragment>| -> Bool {
        let f = unsafe { f.as_ref() };
        out.borrow_mut().push(frag(cm, f));
        Bool::YES
    });
    tlm.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
        None,
        NSTextLayoutFragmentEnumerationOptions::EnsuresLayout,
        &block,
    );
    drop(block);
    let frags = out.into_inner();
    assert_eq!(frags.iter().map(|f| f.range).collect::<Vec<_>>(), [(0, 8), (8, 19), (19, 23)]);
    assert_eq!(frags.iter().map(|f| f.lines.len()).collect::<Vec<_>>(), [2, 2, 1]);
    let (a, b) = (&frags[0], &frags[1]);
    assert_eq!(a.lines.iter().map(|l| l.chars).collect::<Vec<_>>(), [(0, 4), (4, 4)]);
    assert!(near(a.lines[1].typo.origin.y, max_y(a.lines[0].typo)));
    assert!(near(b.frame.origin.y, max_y(a.frame)));
}

// Review fixes: empty documents, ranges after a long paragraph, frames a
// subclass moves, segment options, line fragment edges, elements for a
// range, shorter substitutes, the viewport's relocation, rendering
// attributes, the enumeration delegate, transactions and navigation.

/// A text segment: its range (if asked for), frame and baseline.
type Segment = (Option<(isize, isize)>, NSRect, f64);

/// The segments of `r` of `kind` with `opts`.
fn segs(
    d: &Doc,
    r: &NSTextRange,
    kind: NSTextLayoutManagerSegmentType,
    opts: NSTextLayoutManagerSegmentOptions,
) -> Vec<Segment> {
    let out = RefCell::new(Vec::new());
    let cm = d.cm();
    let block =
        RcBlock::new(|range: *mut NSTextRange, frame: NSRect, baseline: f64, _c: NonNull<NSTextContainer>| -> Bool {
            let r = unsafe { range.as_ref() }.map(|r| rng(cm, r));
            out.borrow_mut().push((r, frame, baseline));
            Bool::YES
        });
    d.tlm.enumerateTextSegmentsInRange_type_options_usingBlock(r, kind, opts, &block);
    drop(block);
    out.into_inner()
}

fn max_x(r: NSRect) -> f64 {
    r.origin.x + r.size.width
}

/// The ranges of the selections a click at (`x`, `y`) makes.
fn click(d: &Doc, x: f64, y: f64) -> Vec<Vec<(isize, isize)>> {
    let nav = d.tlm.textSelectionNavigation();
    let found = nav.textSelectionsInteractingAtPoint_inContainerAtLocation_anchors_modifiers_selecting_bounds(
        NSPoint::new(x, y),
        &d.cs.documentRange().location(),
        &NSArray::new(),
        NSTextSelectionNavigationModifier(0),
        true,
        NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 1000.0)),
    );
    found.iter().map(|s| s.textRanges().iter().map(|r| rng(d.cm(), &r)).collect()).collect()
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextKit2TestTopViewport"]
    #[ivars = RefCell<Vec<((isize, isize), NSRect, usize)>>]
    struct TopViewport;

    unsafe impl NSObjectProtocol for TopViewport {}
    unsafe impl NSTextViewportLayoutControllerDelegate for TopViewport {
        #[unsafe(method(viewportBoundsForTextViewportLayoutController:))]
        fn bounds(&self, _c: &NSTextViewportLayoutController) -> NSRect {
            NSRect::new(NSPoint::ZERO, NSSize::new(200.0, 100.0))
        }

        #[unsafe(method(textViewportLayoutController:configureRenderingSurfaceForTextLayoutFragment:))]
        fn configure(&self, c: &NSTextViewportLayoutController, f: &NSTextLayoutFragment) {
            let cm = c.textLayoutManager().and_then(|m| m.textContentManager()).expect("a content manager");
            self.ivars().borrow_mut().push((rng(&cm, &f.rangeInElement()), f.layoutFragmentFrame(), f.state().0));
        }
    }
);

fn top_viewport() -> Retained<TopViewport> {
    let this = TopViewport::alloc().set_ivars(RefCell::new(Vec::new()));
    unsafe { msg_send![super(this), init] }
}

/// An empty document has no fragments, but a caret there, laying out its
/// range or its viewport lays out its extra line fragment: at the padding,
/// of no width and a line's height, which the usage bounds are then. A
/// click in it makes no selection.
#[test]
fn an_empty_document_lays_out_its_extra_line() {
    let standard = NSTextLayoutManagerSegmentType::Standard;
    let none = NSTextLayoutManagerSegmentOptions::None;
    let d = Doc::new("", 300.0);
    assert_eq!(d.tlm.usageBoundsForTextContainer(), NSRect::ZERO);
    assert!(d.tlm.textLayoutFragmentForLocation(&d.at(0)).is_none());
    let caret = segs(&d, &d.range(0, 0), standard, none);
    assert_eq!(caret.len(), 1, "{caret:?}");
    let (r, frame, baseline) = caret[0];
    assert_eq!(r, Some((0, 0)));
    assert!(near(frame.origin.x, 5.0) && near(frame.origin.y, 0.0) && near(frame.size.width, 0.0), "{frame:?}");
    assert!(frame.size.height > 0.0 && baseline > 0.0 && baseline < frame.size.height, "{caret:?}");
    let h = frame.size.height;
    let usage = d.tlm.usageBoundsForTextContainer();
    assert!(near(usage.origin.x, 5.0) && near(usage.size.width, 0.0) && near(usage.size.height, h), "{usage:?}");

    let d = Doc::new("", 300.0);
    d.tlm.ensureLayoutForRange(&d.cs.documentRange());
    assert!(near(d.tlm.usageBoundsForTextContainer().size.height, h));

    let d = Doc::new("", 300.0);
    let v = top_viewport();
    let vp = d.tlm.textViewportLayoutController();
    vp.setDelegate(Some(ProtocolObject::from_ref(&*v)));
    vp.layoutViewport();
    let log = v.ivars().borrow().clone();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!((log[0].0, log[0].2), ((0, 0), 3));
    assert!(near(log[0].1.origin.x, 5.0) && near(log[0].1.size.height, h), "{log:?}");
    assert_eq!(vp.viewportRange().map(|r| rng(d.cm(), &r)), Some((0, 0)));
    assert!(click(&d, 50.0, 5.0).is_empty());
}

/// Laying out a range of short paragraphs after a long one lays each of
/// them out, and a selection over them has a segment for each.
#[test]
fn ensuring_layout_after_a_long_paragraph() {
    let mut text = String::from("a\n");
    text.push_str(&"y".repeat(400));
    text.push('\n');
    let mut starts = vec![0, 2];
    for _ in 2..400 {
        starts.push(text.len());
        text.push_str("short line\n");
    }
    let (a, b) = (starts[200] as isize, starts[220] as isize);
    let d = Doc::new(&text, 1.0e7);
    d.tlm.ensureLayoutForRange(&d.range(a, b));
    let (frags, _) = d.fragments(NSTextLayoutFragmentEnumerationOptions::None, Some(a));
    let inside: Vec<&Frag> = frags.iter().filter(|f| f.range.0 < b).collect();
    assert_eq!(inside.len(), 20);
    assert!(inside.iter().all(|f| f.state == 3), "{:?}", inside.iter().map(|f| f.state).collect::<Vec<_>>());
    let d = Doc::new(&text, 1.0e7);
    let sel =
        segs(&d, &d.range(a, b), NSTextLayoutManagerSegmentType::Selection, NSTextLayoutManagerSegmentOptions::None);
    assert_eq!(sel.len(), 20);
}

define_class!(
    #[unsafe(super(NSTextLayoutFragment))]
    #[name = "TextKit2TestShiftFragment"]
    struct ShiftFragment;

    impl ShiftFragment {
        #[unsafe(method(layoutFragmentFrame))]
        fn frame(&self) -> NSRect {
            let mut f: NSRect = unsafe { msg_send![super(self), layoutFragmentFrame] };
            f.origin.x += 20.0;
            f.size.height += 10.0;
            f
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextKit2TestShiftFragments"]
    struct ShiftFragments;

    unsafe impl NSObjectProtocol for ShiftFragments {}
    unsafe impl NSTextLayoutManagerDelegate for ShiftFragments {
        #[unsafe(method_id(textLayoutManager:textLayoutFragmentForLocation:inTextElement:))]
        fn fragment(
            &self,
            _tlm: &NSTextLayoutManager,
            _location: &ProtocolObject<dyn NSTextLocation>,
            element: &NSTextElement,
        ) -> Retained<NSTextLayoutFragment> {
            let range = element.elementRange();
            let this = ShiftFragment::alloc().set_ivars(());
            let f: Retained<ShiftFragment> =
                unsafe { msg_send![super(this), initWithTextElement: element, range: range.as_deref()] };
            Retained::into_super(f)
        }
    }
);

/// A subclass's frame, moved right and taller than its lines, is where
/// its lines are: a caret goes by it, a click at its left finds its start,
/// and one in its padding below its line the end of that line.
#[test]
fn a_moved_frame_moves_carets_and_clicks() {
    let d = Doc::new("abcdef\nghijkl\nmnop", 300.0);
    let delegate: Retained<ShiftFragments> = unsafe { msg_send![super(ShiftFragments::alloc().set_ivars(())), init] };
    d.tlm.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let laid = d.laid();
    let f = &laid[1];
    assert_eq!(f.range, (7, 14));
    assert!(f.frame.origin.x > 20.0, "moved: {f:?}");
    let line = f.object.textLineFragments().objectAtIndex(0);
    let caret =
        segs(&d, &d.range(8, 8), NSTextLayoutManagerSegmentType::Standard, NSTextLayoutManagerSegmentOptions::None);
    let at = caret[0].1;
    let want = f.frame.origin.x + line.typographicBounds().origin.x + line.locationForCharacterAtIndex(1).x;
    assert!((at.origin.x - want).abs() < 0.5, "caret {at:?}, fragment {f:?}");
    assert!(near(at.origin.y, f.frame.origin.y), "{at:?} {f:?}");
    assert_eq!(click(&d, f.frame.origin.x + 1.0, f.frame.origin.y + 3.0), [vec![(7, 7)]]);
    let pad = max_y(f.frame) - 2.0;
    assert_eq!(click(&d, 10.0, pad), [vec![(13, 13)]]);
    let there = d.tlm.textLayoutFragmentForPosition(NSPoint::new(10.0, pad)).expect("a fragment");
    assert_eq!(rng(d.cm(), &there.rangeInElement()), (7, 14));
}

/// Segment options, over a paragraph wrapping into lines and a short one
/// after it, as they relate: which lines have a segment, which reach the
/// container's edges (its padding in from each), and where they start
/// down.
#[test]
fn segment_options() {
    type O = NSTextLayoutManagerSegmentOptions;
    type T = NSTextLayoutManagerSegmentType;
    let text = "Hello world this wraps across the line\nnext";
    let d = Doc::new(text, 100.0);
    let laid = d.laid();
    let lines: Vec<(isize, isize)> =
        laid[0].lines.iter().map(|l| (l.chars.0 as isize, (l.chars.0 + l.chars.1) as isize)).collect();
    assert!(lines.len() >= 3, "wraps: {lines:?}");
    let (edge_l, edge_r) = (5.0, 95.0);
    // A range from inside the first line to inside the last paragraph.
    let (a, b) = (2, 42);
    let mut want: Vec<(isize, isize)> = lines.iter().map(|&(s, e)| (s.max(a), e)).collect();
    want.push((39, b));
    let ranges = |v: &[Segment]| v.iter().map(|s| s.0.expect("a range")).collect::<Vec<_>>();

    let std = segs(&d, &d.range(a, b), T::Standard, O::None);
    assert_eq!(ranges(&std), want);
    assert!(std[0].1.origin.x > edge_l && std[1..].iter().all(|s| near(s.1.origin.x, edge_l)), "{std:?}");
    assert!(max_x(std.last().expect("segments").1) < edge_r);

    // A selection: to the trailing edge (no further) on every line it goes
    // on past, from the leading edge after the first, each where the one
    // before it ends.
    let sel = segs(&d, &d.range(a, b), T::Selection, O::None);
    assert_eq!(ranges(&sel), want);
    let n = sel.len();
    for (i, s) in sel.iter().enumerate() {
        if i + 1 < n {
            assert!(near(max_x(s.1), edge_r), "{i}: {sel:?}");
        }
        if i > 0 {
            assert!(near(s.1.origin.x, edge_l) && near(s.1.origin.y, max_y(sel[i - 1].1)), "{i}: {sel:?}");
        }
    }
    assert!(max_x(sel[n - 1].1) < edge_r);

    // The middle lines left out: the first and last (a selection's last
    // reaching up to the first).
    let ends = vec![want[0], want[want.len() - 1]];
    assert_eq!(ranges(&segs(&d, &d.range(a, b), T::Standard, O::MiddleFragmentsExcluded)), ends);
    let sel = segs(&d, &d.range(a, b), T::Selection, O::MiddleFragmentsExcluded);
    assert_eq!(ranges(&sel), ends);
    assert!(near(sel[1].1.origin.y, max_y(sel[0].1)) && near(max_y(sel[1].1), max_y(std[n - 1].1)), "{sel:?}");

    // The tail extended: the lines the range goes on past reach the edge,
    // and past its end the rest of its paragraph's last line does, an empty
    // segment at the line's end from where its text ends.
    let tail = segs(&d, &d.range(a, b), T::Standard, O::TailSegmentExtended);
    assert_eq!(ranges(&tail)[..n], want[..]);
    assert!(tail[1..n - 1].iter().all(|s| near(max_x(s.1), edge_r)), "{tail:?}");
    let end_caret = segs(&d, &d.range(43, 43), T::Standard, O::None);
    assert_eq!(tail.len(), n + 1);
    let extra = tail[n];
    assert_eq!(extra.0, Some((43, 43)));
    assert!(near(extra.1.origin.x, end_caret[0].1.origin.x) && near(max_x(extra.1), edge_r), "{tail:?}");
    assert!(near(extra.1.origin.y, tail[n - 1].1.origin.y));

    // No range asked for: nil.
    let bare = segs(&d, &d.range(2, 5), T::Standard, O::RangeNotRequired);
    assert_eq!(bare.len(), 1);
    assert!(bare[0].0.is_none());

    // The head extended: every line's segment but the first starts at the
    // leading edge, an indented one too.
    let style = NSMutableParagraphStyle::new();
    style.setHeadIndent(20.0);
    style.setFirstLineHeadIndent(20.0);
    let storage = storage_with("plain\n", None);
    storage.appendAttributedString(&attributed("indented words that wrap around\nx", Some(&style)));
    let d = Doc::new("", 120.0);
    d.cs.setTextStorage(Some(&storage));
    let plain = segs(&d, &d.range(2, 12), T::Standard, O::None);
    let head = segs(&d, &d.range(2, 12), T::Standard, O::HeadSegmentExtended);
    assert!(plain[1].1.origin.x > 20.0 && near(head[1].1.origin.x, edge_l), "{plain:?} {head:?}");
    assert!(near(head[0].1.origin.x, plain[0].1.origin.x));
}

/// A line fragment's character at a point: the one under it, none past
/// the line's end; the line at a height, not exactly: the first whose
/// bottom is below it. No fragment holds the document's end.
#[test]
fn line_fragment_edges() {
    let d = Doc::new("first line\nsecond line\nthird", 300.0);
    d.tlm.ensureLayoutForRange(&d.cs.documentRange());
    assert!(d.tlm.textLayoutFragmentForLocation(&d.at(28)).is_none());
    let f = d.tlm.textLayoutFragmentForLocation(&d.at(12)).expect("a fragment");
    let l0 = f.textLineFragments().objectAtIndex(0);
    let h = l0.typographicBounds().size.height;
    let end = l0.locationForCharacterAtIndex(11).x;
    assert_eq!(l0.characterIndexForPoint(NSPoint::new(end - 1.0, h / 2.0)), 10);
    assert_eq!(l0.characterIndexForPoint(NSPoint::new(end + 1.0, h / 2.0)), isize::MAX);
    assert_eq!(l0.characterIndexForPoint(NSPoint::new(-50.0, h / 2.0)), 0);

    let d = Doc::new("MMMM MMMM MMMM MMMM MMMM\nx", 80.0);
    d.tlm.ensureLayoutForRange(&d.cs.documentRange());
    let f = d.tlm.textLayoutFragmentForLocation(&d.at(0)).expect("a fragment");
    let lines = f.textLineFragments();
    assert!(lines.count() >= 2);
    let inexact =
        |y: f64| f.textLineFragmentForVerticalOffset_requiresExactMatch(y, false).map(|l| l.characterRange().location);
    assert_eq!(inexact(-5.0), Some(0));
    for l in lines.iter() {
        let b = l.typographicBounds();
        assert_eq!(inexact(max_y(b) - 0.5), Some(l.characterRange().location));
    }
    assert_eq!(inexact(1.0e4), None);
}

/// The elements for a range: those starting inside it, up to the first
/// that doesn't (the first too). An element made for an attributed string
/// has no content manager or range yet.
#[test]
fn elements_for_a_range_start_inside_it() {
    let d = Doc::new("ab\ncd\n", 300.0);
    let found = |a, b| {
        d.cs.textElementsForRange(&d.range(a, b))
            .iter()
            .map(|e| e.elementRange().map(|r| rng(d.cm(), &r)))
            .collect::<Vec<_>>()
    };
    assert_eq!(found(2, 4), []);
    assert_eq!(found(3, 3), []);
    assert_eq!(found(3, 4), [Some((3, 6))]);
    assert_eq!(found(0, 3), [Some((0, 3))]);
    assert_eq!(found(0, 6), [Some((0, 3)), Some((3, 6))]);
    let e = d.cs.textElementForAttributedString(&attributed("zz\n", None)).expect("an element");
    assert!(e.textContentManager().is_none() && e.elementRange().is_none());
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextKit2TestShorter"]
    struct Shorter;

    unsafe impl NSObjectProtocol for Shorter {}
    unsafe impl NSTextContentManagerDelegate for Shorter {}
    unsafe impl NSTextContentStorageDelegate for Shorter {
        #[unsafe(method_id(textContentStorage:textParagraphWithRange:))]
        fn paragraph(&self, _cs: &NSTextContentStorage, range: NSRange) -> Option<Retained<NSTextParagraph>> {
            (range.location == 4).then(|| {
                NSTextParagraph::initWithAttributedString(NSTextParagraph::alloc(), Some(&attributed("Z\n", None)))
            })
        }
    }
);

/// A delegate's paragraph shorter than the one it stands for: its range is
/// the storage paragraph's, which its content and separator ranges
/// divide; its fragment, laid out, covers only its own text, and no
/// fragment holds the rest (before layout, the paragraph's).
#[test]
fn a_shorter_delegate_paragraph() {
    let d = Doc::new("one\ntwo\nthree\nfour", 300.0);
    let s: Retained<Shorter> = unsafe { msg_send![super(Shorter::alloc().set_ivars(())), init] };
    unsafe { d.cs.setDelegate(Some(ProtocolObject::from_ref(&*s))) };
    let (all, _) = d.elements(None, NSTextContentManagerEnumerationOptions::None, usize::MAX);
    assert_eq!(all.iter().map(|e| e.1).collect::<Vec<_>>(), [(0, 4), (4, 8), (8, 14), (14, 18)]);
    let p = all[1].0.downcast_ref::<NSTextParagraph>().expect("a paragraph");
    assert_eq!(p.paragraphContentRange().map(|r| rng(d.cm(), &r)), Some((4, 7)));
    assert_eq!(p.paragraphSeparatorRange().map(|r| rng(d.cm(), &r)), Some((7, 8)));
    let before = d.tlm.textLayoutFragmentForLocation(&d.at(6)).expect("a fragment");
    assert_eq!(rng(d.cm(), &before.rangeInElement()), (4, 8));
    let laid = d.laid();
    assert_eq!(laid.iter().map(|f| f.range).collect::<Vec<_>>(), [(0, 4), (4, 6), (8, 14), (14, 18)]);
    assert!(d.tlm.textLayoutFragmentForLocation(&d.at(6)).is_none());
    let f = d.tlm.textLayoutFragmentForLocation(&d.at(4)).expect("a fragment");
    assert_eq!(rng(d.cm(), &f.rangeInElement()), (4, 6));
    assert_eq!(rng(d.cm(), &p.elementRange().expect("a range")), (4, 8));
}

/// Relocating the viewport moves its bounds to where the fragment at the
/// location is estimated to be and makes its range the empty range there,
/// laying nothing out and asking the delegate nothing. Without a delegate
/// the bounds have no height, and laying out lays out nothing.
#[test]
fn relocating_the_viewport_lays_nothing_out() {
    let text: String = (0..2000).map(|i| format!("line number {i}\n")).collect();
    let d = Doc::new(&text, 300.0);
    let v = Viewport::alloc().set_ivars(RefCell::new(Vec::new()));
    let v: Retained<Viewport> = unsafe { msg_send![super(v), init] };
    let vp = d.tlm.textViewportLayoutController();
    vp.setDelegate(Some(ProtocolObject::from_ref(&*v)));
    let at = 1000 * 16;
    let top = vp.relocateViewportToTextLocation(&d.at(at));
    assert!(top > 1000.0, "{top}");
    assert_eq!(vp.viewportBounds().origin.y, top);
    assert_eq!(vp.viewportRange().map(|r| rng(d.cm(), &r)), Some((at, at)));
    assert!(v.ivars().borrow().is_empty());
    let f = d.tlm.textLayoutFragmentForLocation(&d.at(at)).expect("a fragment");
    assert_ne!(f.state().0, 3);

    let d = Doc::new("a\nb\nc", 300.0);
    let vp = d.tlm.textViewportLayoutController();
    vp.layoutViewport();
    assert!(vp.viewportRange().is_none());
    assert!(d.fragments(NSTextLayoutFragmentEnumerationOptions::None, None).0.iter().all(|f| f.state == 0));
}

/// Rendering attributes: set, added to and removed from ranges; enumerated
/// from a location (the run there cut at it) or back from it; moved by
/// edits before them.
#[test]
fn rendering_attributes() {
    let d = Doc::new("hello world\nsecond line", 300.0);
    let runs = |from: isize, reverse: bool| {
        let out = RefCell::new(Vec::new());
        let block = RcBlock::new(
            |_m: NonNull<NSTextLayoutManager>,
             a: NonNull<NSDictionary<NSString, AnyObject>>,
             r: NonNull<NSTextRange>|
             -> Bool {
                out.borrow_mut().push((rng(d.cm(), unsafe { r.as_ref() }), unsafe { a.as_ref() }.count()));
                Bool::YES
            },
        );
        d.tlm.enumerateRenderingAttributesFromLocation_reverse_usingBlock(&d.at(from), reverse, &block);
        drop(block);
        out.into_inner()
    };
    assert!(runs(0, false).is_empty());
    let red = objc2_app_kit::NSColor::redColor();
    let (fg, bg) =
        unsafe { (objc2_app_kit::NSForegroundColorAttributeName, objc2_app_kit::NSBackgroundColorAttributeName) };
    let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&[fg], &[&*red as &AnyObject]);
    unsafe { d.tlm.setRenderingAttributes_forTextRange(&attrs, &d.range(2, 8)) };
    assert_eq!(runs(0, false), [((2, 8), 1)]);
    assert_eq!(runs(5, false), [((5, 8), 1)]);
    assert_eq!(runs(10, true), [((2, 8), 1)]);
    let blue = objc2_app_kit::NSColor::blueColor();
    unsafe { d.tlm.addRenderingAttribute_value_forTextRange(bg, Some(&blue), &d.range(6, 12)) };
    assert_eq!(runs(0, false), [((2, 6), 1), ((6, 8), 2), ((8, 12), 1)]);
    d.tlm.removeRenderingAttribute_forTextRange(fg, &d.range(0, 4));
    assert_eq!(runs(0, false), [((4, 6), 1), ((6, 8), 2), ((8, 12), 1)]);
    let ts = d.cs.textStorage().expect("a storage");
    ts.replaceCharactersInRange_withString(NSRange::new(0, 0), &NSString::from_str("XY"));
    assert_eq!(runs(0, false), [((6, 8), 1), ((8, 10), 2), ((10, 14), 1)]);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextKit2TestSkip"]
    #[ivars = RefCell<Vec<(isize, isize, usize)>>]
    struct Skip;

    unsafe impl NSObjectProtocol for Skip {}
    unsafe impl NSTextContentManagerDelegate for Skip {
        #[unsafe(method(textContentManager:shouldEnumerateTextElement:options:))]
        fn should(
            &self,
            cm: &NSTextContentManager,
            e: &NSTextElement,
            options: NSTextContentManagerEnumerationOptions,
        ) -> bool {
            let r = rng(cm, &e.elementRange().expect("a range"));
            self.ivars().borrow_mut().push((r.0, r.1, options.0));
            r.0 != 3
        }
    }
    unsafe impl NSTextContentStorageDelegate for Skip {}
);

/// Enumerating elements asks the delegate of each, with the options, and
/// hands the block only those it wants.
#[test]
fn enumerating_asks_the_delegate() {
    let d = Doc::new("ab\ncd\nef", 300.0);
    let skip: Retained<Skip> = unsafe { msg_send![super(Skip::alloc().set_ivars(RefCell::new(Vec::new()))), init] };
    unsafe { d.cs.setDelegate(Some(ProtocolObject::from_ref(&*skip))) };
    let (seen, ret) = d.elements(None, NSTextContentManagerEnumerationOptions::None, usize::MAX);
    assert_eq!(seen.iter().map(|e| e.1).collect::<Vec<_>>(), [(0, 3), (6, 8)]);
    assert_eq!(ret, Some(8));
    assert_eq!(*skip.ivars().borrow(), [(0, 3, 0), (3, 6, 0), (6, 8, 0)]);
    skip.ivars().borrow_mut().clear();
    let (seen, _) = d.elements(Some(8), NSTextContentManagerEnumerationOptions::Reverse, usize::MAX);
    assert_eq!(seen.iter().map(|e| e.1).collect::<Vec<_>>(), [(6, 8), (0, 3)]);
    assert_eq!(*skip.ivars().borrow(), [(6, 8, 1), (3, 6, 1), (0, 3, 1)]);
}

/// An editing transaction is the content storage's alone: the storage
/// processes each edit in it as it comes (as it does outside one).
#[test]
fn editing_transactions_leave_the_storage_alone() {
    let d = Doc::new("abc\ndef", 300.0);
    let ts = d.cs.textStorage().expect("a storage");
    let processed = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = processed.clone();
    let observer = RcBlock::new(move |_n: NonNull<objc2_foundation::NSNotification>| count.set(count.get() + 1));
    let center = objc2_foundation::NSNotificationCenter::defaultCenter();
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(objc2_app_kit::NSTextStorageDidProcessEditingNotification),
            Some(&ts),
            None,
            &observer,
        )
    };
    let inside = std::cell::Cell::new((0, false));
    let edits = RcBlock::new(|| {
        ts.replaceCharactersInRange_withString(NSRange::new(0, 0), &NSString::from_str("X"));
        ts.replaceCharactersInRange_withString(NSRange::new(5, 0), &NSString::from_str("Y"));
        inside.set((processed.get(), d.cs.hasEditingTransaction()));
    });
    d.cs.performEditingTransactionUsingBlock(&edits);
    assert_eq!(inside.get(), (2, true));
    assert!(!d.cs.hasEditingTransaction());
    assert_eq!(ts.string().to_string(), "Xabc\nYdef");
    let token: &AnyObject = unsafe { &*(Retained::as_ptr(&token) as *const AnyObject) };
    unsafe { center.removeObserver(token) };
}

/// Moving and extending a selection, and what deleting one removes, by
/// character, word, line (the text's first line is its first paragraph)
/// and paragraph.
#[test]
fn navigation_moves() {
    use objc2_app_kit::{NSTextSelectionNavigationDestination as Dest, NSTextSelectionNavigationDirection as Dir};
    let d = Doc::new("hello brave world\nsecond line here", 300.0);
    d.tlm.ensureLayoutForRange(&d.cs.documentRange());
    let nav = d.tlm.textSelectionNavigation();
    let sel = |a: isize, b: isize| {
        NSTextSelection::initWithRange_affinity_granularity(
            NSTextSelection::alloc(),
            &d.range(a, b),
            NSTextSelectionAffinity::Downstream,
            NSTextSelectionGranularity::Character,
        )
    };
    let moved = |(a, b): (isize, isize), dir: Dir, dest: Dest, extend: bool| {
        let s = nav
            .destinationSelectionForTextSelection_direction_destination_extending_confined(
                &sel(a, b),
                dir,
                dest,
                extend,
                false,
            )
            .expect("a selection");
        let r: Vec<(isize, isize)> = s.textRanges().iter().map(|r| rng(d.cm(), &r)).collect();
        (r[0], s.affinity() == NSTextSelectionAffinity::Upstream)
    };
    let (up, down) = (true, false);
    assert_eq!(moved((8, 8), Dir::Forward, Dest::Character, false), ((9, 9), down));
    assert_eq!(moved((8, 12), Dir::Forward, Dest::Character, false), ((12, 12), down));
    assert_eq!(moved((8, 12), Dir::Backward, Dest::Character, false), ((8, 8), down));
    assert_eq!(moved((8, 12), Dir::Forward, Dest::Character, true), ((8, 13), down));
    assert_eq!(moved((8, 12), Dir::Backward, Dest::Character, true), ((8, 11), down));
    assert_eq!(moved((8, 8), Dir::Forward, Dest::Word, false), ((11, 11), down));
    assert_eq!(moved((8, 12), Dir::Forward, Dest::Word, false), ((17, 17), down));
    assert_eq!(moved((8, 8), Dir::Backward, Dest::Word, false), ((6, 6), down));
    assert_eq!(moved((8, 12), Dir::Backward, Dest::Word, true), ((6, 12), up));
    assert_eq!(moved((8, 8), Dir::Forward, Dest::Line, false), ((17, 17), up));
    assert_eq!(moved((8, 12), Dir::Forward, Dest::Line, true), ((8, 17), down));
    assert_eq!(moved((8, 8), Dir::Backward, Dest::Line, false), ((0, 0), down));
    assert_eq!(moved((8, 8), Dir::Forward, Dest::Paragraph, false), ((17, 17), down));
    assert_eq!(moved((8, 8), Dir::Forward, Dest::Document, false), ((34, 34), down));
    assert_eq!(moved((8, 8), Dir::Down, Dest::Word, false), ((11, 11), down));
    assert_eq!(moved((8, 8), Dir::Up, Dest::Character, false), ((0, 0), down));
    assert_eq!(moved((8, 12), Dir::Up, Dest::Character, true), ((0, 8), down));

    let deleted = |(a, b): (isize, isize), dir: Dir, dest: Dest| {
        nav.deletionRangesForTextSelection_direction_destination_allowsDecomposition(&sel(a, b), dir, dest, false)
            .iter()
            .map(|r| rng(d.cm(), &r))
            .collect::<Vec<_>>()
    };
    assert_eq!(deleted((8, 8), Dir::Forward, Dest::Character), [(8, 9)]);
    assert_eq!(deleted((8, 8), Dir::Backward, Dest::Character), [(7, 8)]);
    assert_eq!(deleted((8, 8), Dir::Forward, Dest::Word), [(8, 11)]);
    assert_eq!(deleted((8, 8), Dir::Backward, Dest::Word), [(6, 8)]);
    assert_eq!(deleted((8, 8), Dir::Forward, Dest::Line), [(8, 17)]);
    assert_eq!(deleted((8, 8), Dir::Backward, Dest::Paragraph), [(0, 8)]);
    assert_eq!(deleted((8, 12), Dir::Backward, Dest::Word), [(8, 12)]);
}

/// A fragment draws into a `CGContext` that isn't the current graphics
/// context: a program's own bitmap context, flipped as a flipped view is.
#[test]
fn a_fragment_draws_into_a_bitmap_context() {
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGColorSpace, CGContext,
        CGImageAlphaInfo, kCGColorSpaceSRGB,
    };
    use objc2_foundation::NSPoint as CGPoint;

    let d = Doc::new("Hello", 200.0);
    let f = d.tlm.textLayoutFragmentForLocation(&d.at(0)).expect("a fragment");
    let (w, h) = (120usize, 40usize);
    // SAFETY: the constant is CoreGraphics'.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).expect("sRGB");
    // SAFETY: CoreGraphics allocates the pixels; RGBA with premultiplied
    // alpha is a layout it takes.
    let cg = unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, Some(&space), CGImageAlphaInfo::PremultipliedLast.0)
    }
    .expect("a bitmap context");
    CGContext::translate_ctm(Some(&cg), 0.0, h as f64);
    CGContext::scale_ctm(Some(&cg), 1.0, -1.0);
    let painted = |cg: &CGContext| {
        let row = CGBitmapContextGetBytesPerRow(Some(cg));
        let data = CGBitmapContextGetData(Some(cg)).cast::<u8>();
        // SAFETY: the context's pixels, `row` bytes by `h` rows, RGBA.
        let bytes = unsafe { std::slice::from_raw_parts(data, row * h) };
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| bytes[y * row + x * 4 + 3] > 0).count()
    };
    assert_eq!(painted(&cg), 0);
    // Not laid out yet: drawing lays it out first.
    assert_eq!(f.state(), objc2_app_kit::NSTextLayoutFragmentState::None);
    f.drawAtPoint_inContext(CGPoint::new(4.0, 4.0), &cg);
    assert_eq!(f.state(), objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable);
    assert!(painted(&cg) > 20, "the text's glyphs drew into the bitmap context");
}
