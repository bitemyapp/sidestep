//! NSTextStorage fixing attributes lazily: which edits are fixed as they
//! are processed and which are left to fix, what a delegate hears of each,
//! and how much a question about an index fixes. Expected values are what
//! macOS does (AppKit's own storage fixes lazily: an edit of 65 536 units
//! or more, or any edit while text is left to fix, is fixed when its
//! attributes are asked for, a stretch at a time). The tests run on the
//! test harness's worker threads, as the text storage tests do.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_app_kit::{
    NSFontAttributeName, NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSTextStorage, NSTextStorageDelegate,
    NSTextStorageEditActions,
};
use objc2_foundation::{
    NSAttributedString, NSDictionary, NSInteger, NSMutableAttributedString, NSObjectProtocol, NSRange, NSString,
};

use sidestep as _;

type Dict = NSDictionary<NSString, AnyObject>;

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

fn pair(r: NSRange) -> (usize, usize) {
    (r.location, r.length)
}

/// Lines of 24 units, `len` units of them (the last one cut short).
fn lines(len: usize) -> Retained<NSString> {
    let line = "line of the text 123456\n";
    let mut t = line.repeat(len / line.len() + 1);
    t.truncate(len);
    s(&t)
}

fn keys(d: &Dict) -> Vec<String> {
    let mut k: Vec<String> = d.allKeys().iter().map(|k| k.to_string()).collect();
    k.sort();
    k
}

fn font() -> Vec<String> {
    vec!["NSFont".to_string()]
}

/// The attributes at `i` and their effective range.
fn at(ts: &NSTextStorage, i: usize) -> (Vec<String>, (usize, usize)) {
    let mut r = NSRange::new(0, 0);
    let d = unsafe { ts.attributesAtIndex_effectiveRange(i, &mut r) };
    (keys(&d), pair(r))
}

fn replace(ts: &NSTextStorage, loc: usize, len: usize, text: &NSString) {
    let m: &NSMutableAttributedString = ts;
    m.replaceCharactersInRange_withString(NSRange::new(loc, len), text);
}

fn set_nil(ts: &NSTextStorage, loc: usize, len: usize) {
    let m: &NSMutableAttributedString = ts;
    unsafe { m.setAttributes_range(None, NSRange::new(loc, len)) };
}

/// A storage holding `len` units of lines, none of them fixed yet.
fn left_to_fix(len: usize) -> Retained<NSTextStorage> {
    let ts = NSTextStorage::new();
    replace(&ts, 0, 0, &lines(len));
    ts
}

/// A storage holding `len` units of lines, all fixed.
fn fixed(len: usize) -> Retained<NSTextStorage> {
    let ts = left_to_fix(len);
    ts.ensureAttributesAreFixedInRange(NSRange::new(0, len));
    ts
}

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn log(e: String) {
    LOG.with(|l| l.borrow_mut().push(e));
}

fn take() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

define_class!(
    /// Logs what processing tells it; with `reads`, the attribute keys at
    /// the text's last unit at each call.
    #[unsafe(super(NSObject))]
    #[name = "TextFixingTestDelegate"]
    #[ivars = bool]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSTextStorageDelegate for Delegate {
        #[unsafe(method(textStorage:willProcessEditing:range:changeInLength:))]
        fn will(&self, ts: &NSTextStorage, mask: NSTextStorageEditActions, r: NSRange, delta: NSInteger) {
            log(format!("will {} {:?} {delta}", mask.0, pair(r)));
            if *self.ivars() {
                log(format!("will reads {:?}", at(ts, ts.length() - 1).0));
            }
        }

        #[unsafe(method(textStorage:didProcessEditing:range:changeInLength:))]
        fn did(&self, ts: &NSTextStorage, mask: NSTextStorageEditActions, r: NSRange, delta: NSInteger) {
            log(format!("did {} {:?} {delta}", mask.0, pair(r)));
            if *self.ivars() {
                log(format!("did reads {:?}", at(ts, ts.length() - 1).0));
            }
        }
    }
);

fn delegate(reads: bool) -> Retained<Delegate> {
    unsafe { msg_send![super(Delegate::alloc().set_ivars(reads)), init] }
}

fn watch(ts: &NSTextStorage, d: &Delegate) {
    ts.setDelegate(Some(ProtocolObject::from_ref(d)));
}

/// A subclass keeping its text in an attributed string of its own, which
/// says it fixes lazily or not, logging the fixing asked of it.
struct Kept {
    inner: Retained<NSMutableAttributedString>,
    lazily: bool,
}

define_class!(
    #[unsafe(super(NSTextStorage, NSMutableAttributedString, NSAttributedString, NSObject))]
    #[name = "TextFixingTestKept"]
    #[ivars = Kept]
    struct KeptStorage;

    impl KeptStorage {
        #[unsafe(method_id(string))]
        fn string(&self) -> Retained<NSString> {
            self.ivars().inner.string()
        }

        #[unsafe(method(attributesAtIndex:effectiveRange:))]
        fn attributes(&self, i: usize, r: *mut NSRange) -> *mut Dict {
            let d = unsafe { self.ivars().inner.attributesAtIndex_effectiveRange(i, r) };
            Retained::autorelease_ptr(d)
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace(&self, r: NSRange, text: &NSString) {
            self.ivars().inner.replaceCharactersInRange_withString(r, text);
            let delta = text.length() as isize - r.length as isize;
            let ts: &NSTextStorage = self;
            ts.edited_range_changeInLength(NSTextStorageEditActions::EditedCharacters, r, delta);
        }

        #[unsafe(method(setAttributes:range:))]
        fn set_attributes(&self, attrs: Option<&Dict>, r: NSRange) {
            unsafe { self.ivars().inner.setAttributes_range(attrs, r) };
            let ts: &NSTextStorage = self;
            ts.edited_range_changeInLength(NSTextStorageEditActions::EditedAttributes, r, 0);
        }

        #[unsafe(method(fixesAttributesLazily))]
        fn fixes_lazily(&self) -> bool {
            self.ivars().lazily
        }

        #[unsafe(method(fixAttributesInRange:))]
        fn fix(&self, r: NSRange) {
            log(format!("fix {:?}", pair(r)));
            unsafe { msg_send![super(self), fixAttributesInRange: r] }
        }
    }
);

fn kept(lazily: bool) -> Retained<KeptStorage> {
    let this = KeptStorage::alloc().set_ivars(Kept { inner: NSMutableAttributedString::new(), lazily });
    unsafe { msg_send![super(this), init] }
}

/// AppKit's own storage fixes lazily; NSTextStorage says a subclass with
/// text of its own doesn't.
#[test]
fn fixes_lazily() {
    assert!(NSTextStorage::new().fixesAttributesLazily());
    let sub = kept(true);
    let inherited: bool = unsafe { msg_send![super(&*sub, NSTextStorage::class()), fixesAttributesLazily] };
    assert!(!inherited);
}

/// An edit shorter than 65 536 units is fixed as it is processed (the
/// delegate's did call has the attributes fixing set); a longer one is
/// left to fix, and fixed when its attributes are read, which tells no one.
#[test]
fn long_edits_are_left_to_fix() {
    let d = delegate(false);
    for (len, mask) in [(65_535, 3), (65_536, 2), (70_000, 2)] {
        let ts = NSTextStorage::new();
        watch(&ts, &d);
        replace(&ts, 0, 0, &lines(len));
        assert_eq!(take(), [format!("will 2 (0, {len}) {len}"), format!("did {mask} (0, {len}) {len}")]);
        // Read anywhere, the attributes are fixed.
        for i in [0, len / 2, len - 1] {
            assert_eq!(at(&ts, i).0, font(), "at {i} of {len}");
        }
        assert!(take().is_empty(), "fixing on reading is quiet");
        assert_eq!(ts.editedMask().0, 0);
    }
    // Text with a font of its own keeps it. (AppKit's effective range then
    // ends where it fixed, finding nothing to change; Sidestep's goes on
    // over equal attributes, as an effective range may.)
    let big = objc2_app_kit::NSFont::systemFontOfSize(20.0);
    let key = unsafe { NSFontAttributeName };
    let attrs = Dict::from_slices(&[key], &[&*big as &AnyObject]);
    let text = unsafe { NSAttributedString::new_with_attributes(&lines(100_000), &attrs) };
    let ts = NSTextStorage::new();
    let m: &NSMutableAttributedString = &ts;
    m.replaceCharactersInRange_withAttributedString(NSRange::new(0, 0), &text);
    for i in [0, 50_000, 99_999] {
        let f = unsafe { ts.attribute_atIndex_effectiveRange(key, i, std::ptr::null_mut()) };
        let f = f.and_then(|f| f.downcast::<objc2_app_kit::NSFont>().ok()).expect("a font");
        assert_eq!(f.pointSize(), 20.0);
    }
}

/// What the delegate reads: before fixing (its will call) the text as
/// edited; after (its did call) the attributes fixed, whether fixing was
/// done or left for the read.
#[test]
fn what_the_delegate_reads() {
    let d = delegate(true);
    for (len, mask) in [(480, 3), (100_000, 2)] {
        let ts = NSTextStorage::new();
        watch(&ts, &d);
        replace(&ts, 0, 0, &lines(len));
        assert_eq!(
            take(),
            [
                format!("will 2 (0, {len}) {len}"),
                "will reads []".to_string(),
                format!("did {mask} (0, {len}) {len}"),
                "did reads [\"NSFont\"]".to_string(),
            ]
        );
    }
}

/// A question about an index nearer the start of what is left to fix than
/// its end fixes from that start through the index, 65 536 units at least;
/// otherwise from the index's paragraph through the end. Effective ranges
/// end where the text fixed does.
#[test]
fn fixed_a_stretch_at_a_time() {
    let len = 480_000;
    let ts = left_to_fix(len);
    assert_eq!(at(&ts, 1000), (font(), (0, 65_536)));
    let ts = left_to_fix(len);
    assert_eq!(at(&ts, 100_005).1, (0, 100_006));
    // Just past what was fixed: the next 65 536 units.
    assert_eq!(at(&ts, 100_030).1, (0, 165_542));
    assert_eq!(at(&ts, 99_970).1, (0, 165_542));
    // Nearer the end: from the index's paragraph.
    assert_eq!(at(&ts, 360_010).1, (360_000, 120_000));
    assert_eq!(at(&ts, len - 1).1, (360_000, 120_000));
    assert_eq!(at(&ts, 240_000).1, (0, 240_001));
    // Halfway is nearer the end.
    let ts = left_to_fix(len);
    assert_eq!(at(&ts, len / 2).1, (240_000, 240_000));
    // The last paragraph alone, at the end.
    let ts = left_to_fix(len);
    assert_eq!(at(&ts, len - 5).1, (len - 24, 24));
    // attribute:atIndex:… and the longest effective ranges read the same
    // way, and the longest range ends where the text left to fix starts.
    let ts = left_to_fix(len);
    let mut r = NSRange::new(0, 0);
    let key = unsafe { NSFontAttributeName };
    let value = unsafe { ts.attribute_atIndex_effectiveRange(key, 1000, &mut r) };
    assert_eq!((value.is_some(), pair(r)), (true, (0, 65_536)));
    let all = NSRange::new(0, len);
    let value = unsafe { ts.attribute_atIndex_longestEffectiveRange_inRange(key, 200_000, &mut r, all) };
    assert_eq!((value.is_some(), pair(r)), (true, (0, 200_001)));
    let d = unsafe { ts.attributesAtIndex_longestEffectiveRange_inRange(250_000, &mut r, all) };
    assert_eq!((keys(&d), pair(r)), (font(), (0, 265_537)));
    // A storage made with long text is left to fix too.
    let made: Retained<NSTextStorage> = unsafe { msg_send![NSTextStorage::alloc(), initWithString: &*lines(len)] };
    assert_eq!(at(&made, 1000).1, (0, 65_536));
    let plain = NSAttributedString::from_nsstring(&lines(len));
    let made: Retained<NSTextStorage> = unsafe { msg_send![NSTextStorage::alloc(), initWithAttributedString: &*plain] };
    assert_eq!(at(&made, 1000).1, (0, 65_536));
}

/// ensureAttributesAreFixedInRange: fixes as a question about the range's
/// start does, through the range's end.
#[test]
fn ensuring_a_range() {
    let len = 480_000;
    for ((loc, n), probe, expected) in [
        ((240_000, 10), 240_009, (240_000, 240_000)),
        ((1000, 100_000), 100_999, (0, 101_000)),
        ((400_000, 10), 400_009, (399_984, 80_016)),
        ((239_990, 20), 240_009, (0, 240_010)),
        ((0, 0), 0, (0, 65_536)),
        ((100, 0), 100, (0, 65_536)),
    ] {
        let ts = left_to_fix(len);
        ts.ensureAttributesAreFixedInRange(NSRange::new(loc, n));
        assert_eq!(at(&ts, probe), (font(), expected), "ensure ({loc}, {n})");
    }
    assert_eq!(at(&fixed(len), 1000).1, (0, len));
}

/// While fixing is left for later, the change processed covers the
/// paragraphs it touches; an edit fixed at once reports what it changed.
#[test]
fn a_change_left_to_fix_covers_its_paragraphs() {
    let d = delegate(false);
    for (n, expected) in [(65_535, (10_010, 65_535)), (65_536, (10_008, 65_544)), (70_000, (10_008, 70_008))] {
        let ts = fixed(480_000);
        watch(&ts, &d);
        set_nil(&ts, 10_010, n);
        assert_eq!(take(), [format!("will 1 (10010, {n}) 0"), format!("did 1 {expected:?} 0")]);
        assert_eq!(at(&ts, 10_011).0, font());
    }
    // A short edit while text is left to fix is left to fix too.
    let ts = left_to_fix(480_000);
    watch(&ts, &d);
    set_nil(&ts, 10_010, 4);
    assert_eq!(take(), ["will 1 (10010, 4) 0", "did 1 (10008, 24) 0"]);
    replace(&ts, 30_010, 2, &s("xyz"));
    assert_eq!(take(), ["will 2 (30010, 3) 1", "did 2 (30000, 25) 1"]);
    assert_eq!(at(&ts, 30_011).0, font());
    // What is left to fix moves with the text.
    let ts = left_to_fix(480_000);
    assert_eq!(at(&ts, 479_990).1, (479_976, 24));
    replace(&ts, 100, 100_000, &s(""));
    let len = ts.length();
    assert_eq!(at(&ts, len - 30).1, (len - 48, 48));
    assert_eq!(at(&ts, 10).1, (0, 65_536));
}

/// A paragraph style given to the middle of a paragraph is taken away as
/// the edit is processed, and the change reaches the paragraph's end.
#[test]
fn a_paragraph_restyled_through_its_end() {
    let d = delegate(false);
    let ts = fixed(480);
    watch(&ts, &d);
    let style = NSMutableParagraphStyle::new();
    style.setFirstLineHeadIndent(10.0);
    let m: &NSMutableAttributedString = &ts;
    unsafe { m.addAttribute_value_range(NSParagraphStyleAttributeName, &style, NSRange::new(470, 3)) };
    assert_eq!(take(), ["will 1 (470, 3) 0", "did 1 (470, 10) 0"]);
    assert_eq!(at(&ts, 471), (font(), (0, 480)));
}

/// A subclass that says it fixes lazily is left to fix as AppKit's storage
/// is, whatever the edit's length; ensureAttributesAreFixedInRange: is its
/// to call, and fixes through fixAttributesInRange:, telling no one.
#[test]
fn a_subclass_fixing_lazily() {
    let ts = kept(true);
    let d = delegate(false);
    watch(&ts, &d);
    let text: String = (0..20).map(|i| format!("line {i}\n")).collect();
    replace(&ts, 0, 0, &s(&text));
    assert_eq!(take(), ["will 2 (0, 150) 150", "did 2 (0, 150) 150"]);
    assert_eq!(at(&ts, 3).0, Vec::<String>::new(), "its own reads don't fix");
    replace(&ts, 7, 0, &s("abc\ndef"));
    assert_eq!(take(), ["will 2 (7, 7) 7", "did 2 (7, 14) 7"]);
    ts.ensureAttributesAreFixedInRange(NSRange::new(0, ts.length()));
    assert_eq!(take(), ["fix (0, 157)"]);
    assert_eq!(at(&ts, 3).0, font());
    assert_eq!(at(&ts, 156).0, font());
    // One that doesn't is fixed at once.
    let ts = kept(false);
    watch(&ts, &d);
    replace(&ts, 0, 0, &s(&text));
    assert_eq!(take(), ["will 2 (0, 150) 150", "fix (0, 150)", "did 3 (0, 150) 150"]);
}

/// Text left to fix lays out as the same text fixed does, through edits
/// made while it is left (a change widened to whole paragraphs for the
/// delegate is laid out again as edited).
#[test]
fn layout_of_text_left_to_fix() {
    use objc2_app_kit::{NSLayoutManager, NSTextContainer};
    use objc2_foundation::NSSize;
    let lay_out = |ts: &NSTextStorage| {
        let lm = NSLayoutManager::new();
        let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(150.0, 1.0e7));
        lm.addTextContainer(&tc);
        ts.addLayoutManager(&lm);
        lm.ensureLayoutForCharacterRange(NSRange::new(0, 2000));
        replace(ts, 1010, 0, &s("typed words\nand a new paragraph that wraps in the narrow container "));
        set_nil(ts, 1300, 30);
        let style = NSMutableParagraphStyle::new();
        style.setFirstLineHeadIndent(30.0);
        let m: &NSMutableAttributedString = ts;
        unsafe { m.addAttribute_value_range(NSParagraphStyleAttributeName, &style, NSRange::new(1500, 3)) };
        replace(ts, 1700, 40, &s(""));
        lm.ensureLayoutForCharacterRange(NSRange::new(0, 2500));
        (0..2500)
            .step_by(3)
            .map(|i| {
                let r = unsafe { lm.lineFragmentRectForGlyphAtIndex_effectiveRange(i, std::ptr::null_mut()) };
                let used = unsafe { lm.lineFragmentUsedRectForGlyphAtIndex_effectiveRange(i, std::ptr::null_mut()) };
                (i, r.origin.y, r.size.height, used.origin.x, used.size.width)
            })
            .collect::<Vec<_>>()
    };
    let lazy = left_to_fix(100_000);
    let fixed = fixed(100_000);
    assert_eq!(lay_out(&lazy), lay_out(&fixed));
    assert_eq!(lazy.string().to_string(), fixed.string().to_string());
    for i in [1012, 1302, 1501, 1700] {
        assert_eq!(at(&lazy, i).0, at(&fixed, i).0, "attributes at {i}");
    }
}

define_class!(
    /// Told a change was processed, sets a 40-point font on the range its
    /// ivar holds, once, as a highlighter restyling beyond the edited
    /// paragraph does.
    #[unsafe(super(NSObject))]
    #[name = "TextFixingTestRestyler"]
    #[ivars = std::cell::Cell<Option<NSRange>>]
    struct Restyler;

    unsafe impl NSObjectProtocol for Restyler {}

    unsafe impl NSTextStorageDelegate for Restyler {
        #[unsafe(method(textStorage:didProcessEditing:range:changeInLength:))]
        fn did(&self, ts: &NSTextStorage, _mask: NSTextStorageEditActions, _r: NSRange, _delta: NSInteger) {
            if let Some(r) = self.ivars().take() {
                let big = objc2_app_kit::NSFont::systemFontOfSize(40.0);
                let m: &NSMutableAttributedString = ts;
                unsafe { m.addAttribute_value_range(NSFontAttributeName, &big, r) };
            }
        }
    }
);

/// Attributes a delegate changes as it is told a change was processed, past
/// the edited paragraph, are laid out again, whether the change's fixing
/// was put off or not.
#[test]
fn a_delegates_restyling_is_laid_out() {
    use objc2_app_kit::{NSLayoutManager, NSTextContainer};
    use objc2_foundation::NSSize;
    let heights = |ts: &NSTextStorage| {
        let d: Retained<Restyler> =
            unsafe { msg_send![super(Restyler::alloc().set_ivars(std::cell::Cell::new(None))), init] };
        ts.setDelegate(Some(ProtocolObject::from_ref(&*d)));
        let lm = NSLayoutManager::new();
        let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(400.0, 1.0e7));
        lm.addTextContainer(&tc);
        ts.addLayoutManager(&lm);
        lm.ensureLayoutForCharacterRange(NSRange::new(0, 8000));
        let height =
            |i| unsafe { lm.lineFragmentRectForGlyphAtIndex_effectiveRange(i, std::ptr::null_mut()) }.size.height;
        let before = height(5005);
        d.ivars().set(Some(NSRange::new(5000, 24)));
        replace(ts, 1010, 0, &s("x"));
        lm.ensureLayoutForCharacterRange(NSRange::new(0, 8000));
        let after = height(5006);
        ts.setDelegate(None);
        (before, after)
    };
    let (before, after) = heights(&fixed(100_000));
    assert!(after > before + 10.0, "fixed: {before} then {after}");
    assert_eq!(heights(&left_to_fix(100_000)), (before, after), "left to fix");
}
