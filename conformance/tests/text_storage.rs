//! NSTextStorage: how edits gather into one change, the order
//! `processEditing` tells the notification center, the delegate and the
//! layout managers, the attributes it fixes, effective ranges, the live
//! string, and a subclass that keeps its own text. Expected values are
//! what macOS does. The tests run on the test harness's worker threads,
//! which also shows a text storage needs no main thread.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_app_kit::{
    NSFont, NSFontAttributeName, NSLayoutManager, NSMutableParagraphStyle, NSParagraphStyle,
    NSParagraphStyleAttributeName, NSTextStorage, NSTextStorageDelegate, NSTextStorageEditActions,
};
use objc2_foundation::{
    NSAttributedString, NSCopying, NSDictionary, NSInteger, NSMutableAttributedString, NSNotFound, NSNotification,
    NSNotificationCenter, NSObjectProtocol, NSRange, NSString,
};

use sidestep as _;

type Dict = NSDictionary<NSString, AnyObject>;

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

fn range(loc: usize, len: usize) -> NSRange {
    NSRange::new(loc, len)
}

fn pair(r: NSRange) -> (usize, usize) {
    (r.location, r.length)
}

fn replace(ts: &NSTextStorage, loc: usize, len: usize, text: &str) {
    let m: &NSMutableAttributedString = ts;
    m.replaceCharactersInRange_withString(range(loc, len), &s(text));
}

fn add(ts: &NSTextStorage, key: &NSString, value: &AnyObject, loc: usize, len: usize) {
    let m: &NSMutableAttributedString = ts;
    unsafe { m.addAttribute_value_range(key, value, range(loc, len)) };
}

/// The attributes at `i` and their effective range.
fn at(ts: &NSTextStorage, i: usize) -> (Retained<Dict>, (usize, usize)) {
    let mut r = range(0, 0);
    let d = unsafe { ts.attributesAtIndex_effectiveRange(i, &mut r) };
    (d, pair(r))
}

fn keys(d: &Dict) -> Vec<String> {
    let mut k: Vec<String> = d.allKeys().iter().map(|k| k.to_string()).collect();
    k.sort();
    k
}

#[derive(Default)]
struct Log {
    events: RefCell<Vec<String>>,
}

impl Log {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut self.events.borrow_mut())
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextStorageTestDelegate"]
    #[ivars = Log]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSTextStorageDelegate for Delegate {
        #[unsafe(method(textStorage:willProcessEditing:range:changeInLength:))]
        fn will(&self, ts: &NSTextStorage, mask: NSTextStorageEditActions, r: NSRange, delta: NSInteger) {
            // What the storage says matches what it passes.
            assert_eq!((ts.editedMask(), pair(ts.editedRange()), ts.changeInLength()), (mask, pair(r), delta));
            self.ivars().events.borrow_mut().push(format!("will {} {:?} {delta}", mask.0, pair(r)));
        }

        #[unsafe(method(textStorage:didProcessEditing:range:changeInLength:))]
        fn did(&self, _ts: &NSTextStorage, mask: NSTextStorageEditActions, r: NSRange, delta: NSInteger) {
            self.ivars().events.borrow_mut().push(format!("did {} {:?} {delta}", mask.0, pair(r)));
        }
    }

    impl Delegate {
        #[unsafe(method(note:))]
        fn note(&self, n: &NSNotification) {
            let name = n.name().to_string();
            let short = if name.contains("Will") { "will-note" } else { "did-note" };
            self.ivars().events.borrow_mut().push(short.to_string());
        }
    }
);

define_class!(
    #[unsafe(super(NSLayoutManager, NSObject))]
    #[name = "TextStorageTestLayoutManager"]
    #[ivars = Log]
    struct Manager;

    impl Manager {
        #[unsafe(method(processEditingForTextStorage:edited:range:changeInLength:invalidatedRange:))]
        fn process(&self, ts: &NSTextStorage, mask: NSTextStorageEditActions, r: NSRange, delta: NSInteger, inv: NSRange) {
            self.ivars()
                .events
                .borrow_mut()
                .push(format!("manager {} {:?} {delta} {:?}", mask.0, pair(r), pair(inv)));
            unsafe {
                let _: () = msg_send![super(self), processEditingForTextStorage: ts, edited: mask, range: r, changeInLength: delta, invalidatedRange: inv];
            }
        }
    }
);

fn delegate() -> Retained<Delegate> {
    unsafe { msg_send![super(Delegate::alloc().set_ivars(Log::default())), init] }
}

fn observe(ts: &NSTextStorage, d: &Delegate) {
    let center = NSNotificationCenter::defaultCenter();
    unsafe {
        center.addObserver_selector_name_object(
            d,
            sel!(note:),
            Some(objc2_app_kit::NSTextStorageWillProcessEditingNotification),
            Some(ts),
        );
        center.addObserver_selector_name_object(
            d,
            sel!(note:),
            Some(objc2_app_kit::NSTextStorageDidProcessEditingNotification),
            Some(ts),
        );
    }
}

fn unobserve(d: &Delegate) {
    unsafe { NSNotificationCenter::defaultCenter().removeObserver(d) };
}

#[test]
fn notification_names() {
    unsafe {
        assert_eq!(
            objc2_app_kit::NSTextStorageWillProcessEditingNotification.to_string(),
            "NSTextStorageWillProcessEditingNotification"
        );
        assert_eq!(
            objc2_app_kit::NSTextStorageDidProcessEditingNotification.to_string(),
            "NSTextStorageDidProcessEditingNotification"
        );
    }
}

/// Edits between beginEditing and endEditing make one change: the range
/// edited in the text as it ends up, and the change in length.
#[test]
fn edits_gather_into_one_change() {
    let ts = NSTextStorage::new();
    replace(&ts, 0, 0, "hello world");
    // Processed at once: nothing pending.
    assert_eq!(ts.editedMask(), NSTextStorageEditActions(0));
    assert_eq!(ts.editedRange().location, NSNotFound as usize);
    assert_eq!(ts.changeInLength(), 0);
    ts.beginEditing();
    replace(&ts, 0, 5, "HI");
    assert_eq!((ts.editedMask().0, pair(ts.editedRange()), ts.changeInLength()), (2, (0, 2), -3));
    replace(&ts, 8, 0, "xyz");
    assert_eq!((pair(ts.editedRange()), ts.changeInLength()), ((0, 11), 0));
    replace(&ts, 0, 1, "");
    assert_eq!((ts.editedMask().0, pair(ts.editedRange()), ts.changeInLength()), (2, (0, 10), -1));
    // Nested editing processes once, at the outermost end.
    ts.beginEditing();
    ts.endEditing();
    assert_eq!(ts.editedMask().0, 2);
    ts.endEditing();
    assert_eq!(ts.editedMask().0, 0);
    assert_eq!(ts.string().to_string(), "I worldxyz");
    // Attribute changes gather as attributes.
    ts.beginEditing();
    add(&ts, &s("k"), &s("value-long-enough"), 2, 3);
    assert_eq!((ts.editedMask().0, pair(ts.editedRange()), ts.changeInLength()), (1, (2, 3), 0));
    replace(&ts, 9, 1, "AB");
    assert_eq!((ts.editedMask().0, pair(ts.editedRange()), ts.changeInLength()), (3, (2, 9), 1));
    ts.endEditing();
    // An edit before the gathered range moves it.
    ts.beginEditing();
    replace(&ts, 6, 2, "");
    replace(&ts, 0, 0, "12");
    assert_eq!((pair(ts.editedRange()), ts.changeInLength()), ((0, 8), 0));
    ts.endEditing();
}

/// processEditing: the will notification, the delegate's will, attribute
/// fixing, the did notification, the delegate's did, then each layout
/// manager, with the edited range as the range to lay out again.
#[test]
fn processing_order() {
    let ts = NSTextStorage::new();
    let d = delegate();
    ts.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    observe(&ts, &d);
    let m: Retained<Manager> = unsafe { msg_send![super(Manager::alloc().set_ivars(Log::default())), init] };
    ts.addLayoutManager(&m);
    replace(&ts, 0, 0, "hello world");
    let mut events = d.ivars().take();
    events.extend(m.ivars().take());
    // Fixing gave the new text a font, so the did calls include attributes.
    assert_eq!(
        events,
        ["will-note", "will 2 (0, 11) 11", "did-note", "did 3 (0, 11) 11", "manager 3 (0, 11) 11 (0, 11)"]
    );
    ts.beginEditing();
    replace(&ts, 0, 5, "HI");
    replace(&ts, 8, 0, "xyz");
    assert!(d.ivars().take().is_empty(), "nothing is processed while editing");
    ts.endEditing();
    let mut events = d.ivars().take();
    events.extend(m.ivars().take());
    assert_eq!(events, ["will-note", "will 2 (0, 11) 0", "did-note", "did 2 (0, 11) 0", "manager 2 (0, 11) 0 (0, 11)"]);
    // Attributes only.
    add(&ts, &s("k"), &s("value-long-enough"), 1, 1);
    let mut events = d.ivars().take();
    events.extend(m.ivars().take());
    assert_eq!(events, ["will-note", "will 1 (1, 1) 0", "did-note", "did 1 (1, 1) 0", "manager 1 (1, 1) 0 (1, 1)"]);
    unobserve(&d);
}

/// Text without a font gets one, 12 points; a paragraph takes the
/// paragraph style of its first character throughout.
#[test]
fn fixing_attributes() {
    let ts = NSTextStorage::new();
    replace(&ts, 0, 0, "plain");
    let (d, _) = at(&ts, 0);
    assert_eq!(keys(&d), ["NSFont"]);
    let font = d.objectForKey(unsafe { NSFontAttributeName }).unwrap().downcast::<NSFont>().unwrap();
    assert_eq!(font.pointSize(), 12.0);

    // A style in the middle of a paragraph goes: the first character has
    // none.
    let ts = NSTextStorage::new();
    replace(&ts, 0, 0, "abc def\nghi");
    let style = NSMutableParagraphStyle::new();
    style.setFirstLineHeadIndent(10.0);
    add(&ts, unsafe { NSParagraphStyleAttributeName }, &style, 4, 2);
    assert_eq!(keys(&at(&ts, 4).0), ["NSFont"]);
    assert_eq!(at(&ts, 0).1, (0, 11));
    // One at a paragraph's start covers the paragraph, and only it.
    add(&ts, unsafe { NSParagraphStyleAttributeName }, &style, 0, 2);
    let (d, r) = at(&ts, 5);
    assert_eq!(keys(&d), ["NSFont", "NSParagraphStyle"]);
    assert_eq!(r, (0, 8));
    let got = d.objectForKey(unsafe { NSParagraphStyleAttributeName }).unwrap().downcast::<NSParagraphStyle>().unwrap();
    assert_eq!(got.firstLineHeadIndent(), 10.0);
    assert_eq!(keys(&at(&ts, 9).0), ["NSFont"]);
    // A font set by the program stays.
    let big = NSFont::systemFontOfSize(20.0);
    add(&ts, unsafe { NSFontAttributeName }, &big, 9, 1);
    let f = at(&ts, 9).0.objectForKey(unsafe { NSFontAttributeName }).unwrap().downcast::<NSFont>().unwrap();
    assert_eq!(f.pointSize(), 20.0);
}

/// A storage made with text has its attributes fixed from the start.
#[test]
fn made_with_text() {
    let ts: Retained<NSTextStorage> = unsafe { msg_send![NSTextStorage::alloc(), initWithString: &*s("abc")] };
    assert_eq!(keys(&at(&ts, 0).0), ["NSFont"]);
    let key = s("custom");
    let attrs = Dict::from_slices(&[&*key], &[&*s("value") as &AnyObject]);
    let ts: Retained<NSTextStorage> =
        unsafe { msg_send![NSTextStorage::alloc(), initWithString: &*s("abc"), attributes: &*attrs] };
    assert_eq!(keys(&at(&ts, 1).0), ["NSFont", "custom"]);
    let plain = NSAttributedString::from_nsstring(&s("abc"));
    let ts: Retained<NSTextStorage> = unsafe { msg_send![NSTextStorage::alloc(), initWithAttributedString: &*plain] };
    assert_eq!(keys(&at(&ts, 2).0), ["NSFont"]);
}

/// The scripting accessors: the font and color of the first character,
/// set over all the text.
#[test]
fn font_and_color_of_all_the_text() {
    use objc2_app_kit::NSColor;
    let ts = NSTextStorage::new();
    assert!(ts.respondsToSelector(sel!(font)) && ts.respondsToSelector(sel!(setForegroundColor:)));
    replace(&ts, 0, 0, "ab\ncd");
    assert_eq!(ts.font().map(|f| f.pointSize()), Some(12.0));
    let big = NSFont::systemFontOfSize(20.0);
    ts.setFont(Some(&big));
    assert_eq!(ts.font().map(|f| f.pointSize()), Some(20.0));
    let f = at(&ts, 4).0.objectForKey(unsafe { NSFontAttributeName }).unwrap().downcast::<NSFont>().unwrap();
    assert_eq!((f.pointSize(), at(&ts, 4).1), (20.0, (0, 5)));
    let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    ts.setForegroundColor(Some(&red));
    assert!(ts.foregroundColor().is_some());
    assert_eq!(keys(&at(&ts, 3).0), ["NSColor", "NSFont"]);
}

/// Runs reach across paragraphs; text inserted takes the attributes of
/// what it follows.
#[test]
fn runs_and_inheritance() {
    let ts = NSTextStorage::new();
    replace(&ts, 0, 0, "aa\nbb\ncc");
    add(&ts, &s("k"), &s("value-long-enough"), 0, 8);
    assert_eq!(at(&ts, 4).1, (0, 8));
    add(&ts, &s("x"), &s("other-long-enough"), 3, 2);
    assert_eq!(at(&ts, 0).1, (0, 3));
    assert_eq!(at(&ts, 3).1, (3, 2));
    assert_eq!(at(&ts, 6).1, (5, 3));
    // Inserted after "b", before "\n": the "b"'s attributes.
    replace(&ts, 5, 0, "Z");
    assert_eq!(keys(&at(&ts, 5).0), ["NSFont", "k", "x"]);
    // At the start: the first character's.
    replace(&ts, 0, 0, "Y");
    assert_eq!(keys(&at(&ts, 0).0), ["NSFont", "k"]);
    // Replacing: the first replaced character's.
    replace(&ts, 4, 3, "QQ");
    assert_eq!(keys(&at(&ts, 4).0), ["NSFont", "k", "x"]);
    assert_eq!(ts.string().to_string(), "Yaa\nQQ\ncc");
    // attribute:atIndex:effectiveRange: and removal.
    let m: &NSMutableAttributedString = &ts;
    let mut r = range(0, 0);
    let v = unsafe { ts.attribute_atIndex_effectiveRange(&s("x"), 5, &mut r) };
    assert_eq!(
        v.and_then(|v| v.downcast::<NSString>().ok()).map(|v| v.to_string()).as_deref(),
        Some("other-long-enough")
    );
    m.removeAttribute_range(&s("x"), range(0, ts.length()));
    assert_eq!(keys(&at(&ts, 5).0), ["NSFont", "k"]);
    assert_eq!(at(&ts, 5).1, (0, 9));
}

/// setAttributedString:, append and insert carry the other string's runs.
#[test]
fn attributed_edits() {
    let other = NSMutableAttributedString::from_nsstring(&s("one two"));
    unsafe { other.addAttribute_value_range(&s("k"), &s("value-long-enough"), range(4, 3)) };
    let ts = NSTextStorage::new();
    let m: &NSMutableAttributedString = &ts;
    m.setAttributedString(&other);
    assert_eq!(ts.string().to_string(), "one two");
    assert_eq!(keys(&at(&ts, 5).0), ["NSFont", "k"]);
    assert_eq!(at(&ts, 0).1, (0, 4));
    m.appendAttributedString(&other);
    assert_eq!(ts.string().to_string(), "one twoone two");
    assert_eq!(at(&ts, 12).1, (11, 3));
    m.insertAttributedString_atIndex(&NSAttributedString::from_nsstring(&s("!")), 0);
    assert_eq!(ts.string().to_string(), "!one twoone two");
    assert_eq!(ts.length(), 15);
    // Its own contents, appended to itself.
    m.appendAttributedString(&ts.copy());
    assert_eq!(ts.length(), 30);
    let init: Retained<NSTextStorage> = unsafe { msg_send![NSTextStorage::alloc(), initWithAttributedString: &*other] };
    assert_eq!(init.string().to_string(), "one two");
    assert_eq!(at(&init, 5).1, (4, 3));
}

/// The string is live, and its copies aren't.
#[test]
fn the_string_is_live() {
    let ts = NSTextStorage::new();
    replace(&ts, 0, 0, "a😀b\nc");
    let string = ts.string();
    let snapshot = string.copy();
    replace(&ts, 0, 0, "Z");
    assert_eq!(string.to_string(), "Za😀b\nc");
    assert_eq!(snapshot.to_string(), "a😀b\nc");
    assert_eq!(string.length(), 7);
    assert_eq!(string.characterAtIndex(2), 0xD83D);
    assert_eq!(string.characterAtIndex(3), 0xDE00);
    assert_eq!(string.substringWithRange(range(1, 4)).to_string(), "a😀b");
    let mut units = [0u16; 3];
    unsafe { string.getCharacters_range(std::ptr::NonNull::new(units.as_mut_ptr()).unwrap(), range(3, 3)) };
    assert_eq!(units, [0xDE00, u16::from(b'b'), u16::from(b'\n')]);
    assert!(string.isEqualToString(&s("Za😀b\nc")));
    let m: &NSMutableAttributedString = &ts;
    // Edits through the mutable string reach the storage.
    m.mutableString().appendString(&s("!"));
    assert_eq!(ts.string().to_string(), "Za😀b\nc!");
    assert_eq!(keys(&at(&ts, 7).0), ["NSFont"]);
}

/// Layout managers belong to the storage that adds them.
/// The live string answers what any string with its text does: lines,
/// paragraphs, composed characters, prefixes and suffixes.
#[test]
fn the_string_answers_as_any_string() {
    // Long enough that what is near an index is less than all of it.
    let text = "one\r\ntwo\u{2028}half\u{2029}three\nfour e\u{301}\u{1F600}!\rfive\n".repeat(4);
    let text = text.as_str();
    let ts = NSTextStorage::new();
    replace(&ts, 0, 0, text);
    let live = ts.string();
    let plain = s(text);
    let len = plain.length();
    assert_eq!(live.length(), len);
    let mut ranges = Vec::new();
    for loc in 0..=len {
        for l in [0, 1, 3, 9] {
            if loc + l <= len {
                ranges.push(range(loc, l));
            }
        }
    }
    for r in ranges {
        assert_eq!(pair(live.paragraphRangeForRange(r)), pair(plain.paragraphRangeForRange(r)), "paragraph {r:?}");
        assert_eq!(pair(live.lineRangeForRange(r)), pair(plain.lineRangeForRange(r)), "line {r:?}");
        let get = |st: &NSString, lines: bool| {
            let (mut a, mut b, mut c) = (0, 0, 0);
            unsafe {
                if lines {
                    st.getLineStart_end_contentsEnd_forRange(&mut a, &mut b, &mut c, r)
                } else {
                    st.getParagraphStart_end_contentsEnd_forRange(&mut a, &mut b, &mut c, r)
                }
            };
            (a, b, c)
        };
        assert_eq!(get(&live, true), get(&plain, true), "line parts {r:?}");
        assert_eq!(get(&live, false), get(&plain, false), "paragraph parts {r:?}");
    }
    for i in 0..len {
        assert_eq!(
            pair(live.rangeOfComposedCharacterSequenceAtIndex(i)),
            pair(plain.rangeOfComposedCharacterSequenceAtIndex(i)),
            "composed {i}"
        );
    }
    for t in ["one", "one\r\n", "", "x", "five\n", "\n", text] {
        assert_eq!(live.hasPrefix(&s(t)), plain.hasPrefix(&s(t)), "prefix {t:?}");
        assert_eq!(live.hasSuffix(&s(t)), plain.hasSuffix(&s(t)), "suffix {t:?}");
    }
}

#[test]
fn layout_managers() {
    let ts = NSTextStorage::new();
    let a = NSLayoutManager::new();
    let b = NSLayoutManager::new();
    ts.addLayoutManager(&a);
    ts.addLayoutManager(&b);
    assert_eq!(ts.layoutManagers().count(), 2);
    assert!(unsafe { a.textStorage() }.is_some_and(|t| std::ptr::eq(&*t, &*ts)));
    ts.removeLayoutManager(&a);
    assert_eq!(ts.layoutManagers().count(), 1);
    assert!(unsafe { a.textStorage() }.is_none());
    assert!(unsafe { b.textStorage() }.is_some());
}

/// A subclass keeping its text in an attributed string of its own, as
/// syntax highlighters do: the non-primitive methods and processing go
/// through its primitives.
struct Backed {
    inner: Retained<NSMutableAttributedString>,
    calls: RefCell<Vec<&'static str>>,
}

define_class!(
    #[unsafe(super(NSTextStorage, NSMutableAttributedString, NSAttributedString, NSObject))]
    #[name = "TextStorageTestBacked"]
    #[ivars = Backed]
    struct BackedStorage;

    impl BackedStorage {
        #[unsafe(method_id(string))]
        fn string(&self) -> Retained<NSString> {
            self.ivars().calls.borrow_mut().push("string");
            self.ivars().inner.string()
        }

        #[unsafe(method(attributesAtIndex:effectiveRange:))]
        fn attributes(&self, i: usize, r: *mut NSRange) -> *mut Dict {
            self.ivars().calls.borrow_mut().push("attributes");
            let d = unsafe { self.ivars().inner.attributesAtIndex_effectiveRange(i, r) };
            Retained::autorelease_ptr(d)
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace(&self, r: NSRange, text: &NSString) {
            self.ivars().calls.borrow_mut().push("replace");
            self.ivars().inner.replaceCharactersInRange_withString(r, text);
            let delta = text.length() as isize - r.length as isize;
            let ts: &NSTextStorage = self;
            ts.edited_range_changeInLength(NSTextStorageEditActions::EditedCharacters, r, delta);
        }

        #[unsafe(method(setAttributes:range:))]
        fn set_attributes(&self, attrs: Option<&Dict>, r: NSRange) {
            self.ivars().calls.borrow_mut().push("set");
            unsafe { self.ivars().inner.setAttributes_range(attrs, r) };
            let ts: &NSTextStorage = self;
            ts.edited_range_changeInLength(NSTextStorageEditActions::EditedAttributes, r, 0);
        }
    }
);

#[test]
fn a_subclass_with_its_own_text() {
    let this = BackedStorage::alloc()
        .set_ivars(Backed { inner: NSMutableAttributedString::new(), calls: RefCell::new(Vec::new()) });
    let ts: Retained<BackedStorage> = unsafe { msg_send![super(this), init] };
    let d = delegate();
    ts.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    replace(&ts, 0, 0, "hello\nworld");
    assert_eq!(d.ivars().take(), ["will 2 (0, 11) 11", "did 3 (0, 11) 11"]);
    assert_eq!(ts.length(), 11);
    assert_eq!(ts.ivars().inner.string().to_string(), "hello\nworld");
    // Fixing went through the subclass.
    assert_eq!(keys(&at(&ts, 7).0), ["NSFont"]);
    ts.ivars().calls.borrow_mut().clear();
    add(&ts, &s("k"), &s("value-long-enough"), 0, 3);
    let calls = ts.ivars().calls.borrow().clone();
    assert!(calls.contains(&"set"), "{calls:?}");
    assert_eq!(keys(&at(&ts, 1).0), ["NSFont", "k"]);
    assert_eq!(d.ivars().take(), ["will 1 (0, 3) 0", "did 1 (0, 3) 0"]);
}
