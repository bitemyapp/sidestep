//! NSTextView, built as TextKit 1 (storage, layout manager, container,
//! `initWithFrame:textContainer:`), and the undo machinery around it:
//! defaults, the edit transaction's delegate calls and notifications in
//! order, the editing commands the key bindings send, selection and its
//! granularity, undo and redo of typing, the input client methods, the
//! pasteboard, and a window's undo manager. Expected values are what macOS
//! does.
//!
//! Text is in the monospaced system font, so vertical moves land in the
//! same column on every platform. No window is shown: a view outside a
//! window, or in one never ordered front, isn't first responder of a key
//! window, so editing begins and ends with each edit, as macOS does then.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSFont, NSFontWeightRegular, NSLayoutManager, NSPasteboard,
    NSPasteboardTypeString, NSResponder, NSSelectionGranularity, NSTextContainer, NSTextDelegate, NSTextInputClient,
    NSTextStorage, NSTextView, NSTextViewDelegate, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSDate, NSMutableAttributedString, NSNotFound, NSNotification, NSNumber,
    NSObjectProtocol, NSPoint, NSRange, NSRect, NSRunLoop, NSSize, NSString, NSUndoManager, NSValue,
};

use sidestep as _;

#[derive(Default)]
struct Log {
    events: RefCell<Vec<String>>,
    um: RefCell<Option<Retained<NSUndoManager>>>,
    refuse: RefCell<Option<Sel>>,
    editor: RefCell<Option<Retained<NSTextView>>>,
}

impl Log {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut self.events.borrow_mut())
    }

    fn push(&self, e: String) {
        self.events.borrow_mut().push(e);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextViewTestDelegate"]
    #[ivars = Log]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSTextDelegate for Delegate {
        #[unsafe(method(textShouldBeginEditing:))]
        fn should_begin(&self, _t: &AnyObject) -> bool {
            self.ivars().push("shouldBegin".into());
            true
        }

        #[unsafe(method(textDidBeginEditing:))]
        fn did_begin(&self, n: &NSNotification) {
            assert!(n.object().is_some());
            self.ivars().push("didBegin".into());
        }

        #[unsafe(method(textDidChange:))]
        fn did_change(&self, _n: &NSNotification) {
            self.ivars().push("didChange".into());
        }

        #[unsafe(method(textShouldEndEditing:))]
        fn should_end(&self, _t: &AnyObject) -> bool {
            self.ivars().push("shouldEnd".into());
            true
        }

        #[unsafe(method(textDidEndEditing:))]
        fn did_end(&self, _n: &NSNotification) {
            self.ivars().push("didEnd".into());
        }
    }

    unsafe impl NSTextViewDelegate for Delegate {
        #[unsafe(method(textView:shouldChangeTextInRange:replacementString:))]
        fn should_change(&self, _tv: &NSTextView, r: NSRange, s: Option<&NSString>) -> bool {
            let s = s.map(|s| s.to_string()).unwrap_or_default();
            self.ivars().push(format!("shouldChange {:?} {s:?}", (r.location, r.length)));
            s != "refused"
        }

        #[unsafe(method(textViewDidChangeSelection:))]
        fn did_change_selection(&self, n: &NSNotification) {
            let info = n.userInfo().expect("the old selection");
            let key = NSString::from_str("NSOldSelectedCharacterRange");
            let old =
                info.objectForKey(&key).and_then(|v| v.downcast::<NSValue>().ok()).map(|v| unsafe { v.rangeValue() });
            let old = old.map(|r| (r.location, r.length));
            self.ivars().push(format!("didChangeSelection from {old:?}"));
        }

        #[unsafe(method(textView:willChangeSelectionFromCharacterRange:toCharacterRange:))]
        fn will_change_selection(&self, _tv: &NSTextView, old: NSRange, new: NSRange) -> NSRange {
            self.ivars().push(format!(
                "willChangeSelection {:?} {:?}",
                (old.location, old.length),
                (new.location, new.length)
            ));
            new
        }

        #[unsafe(method_id(undoManagerForTextView:))]
        fn undo_manager_for(&self, _tv: &NSTextView) -> Option<Retained<NSUndoManager>> {
            self.ivars().um.borrow().clone()
        }

        #[unsafe(method(textView:doCommandBySelector:))]
        fn do_command(&self, _tv: &NSTextView, sel: Sel) -> bool {
            self.ivars().push(format!("doCommand {}", sel.name().to_str().unwrap()));
            *self.ivars().refuse.borrow() == Some(sel)
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextViewTestWindowDelegate"]
    #[ivars = Log]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method_id(windowWillReturnUndoManager:))]
        fn window_will_return_undo_manager(&self, _w: &NSWindow) -> Option<Retained<NSUndoManager>> {
            self.ivars().um.borrow().clone()
        }
    }
);

define_class!(
    /// A control of its own that edits in the window's field editor, as a
    /// text field does: the editor as its subview, itself as the editor's
    /// delegate.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextViewTestFieldLike"]
    #[ivars = Log]
    struct FieldLike;

    unsafe impl NSObjectProtocol for FieldLike {}

    impl FieldLike {
        #[unsafe(method(textShouldBeginEditing:))]
        fn should_begin(&self, _t: &AnyObject) -> bool {
            self.ivars().push("shouldBegin".into());
            true
        }

        #[unsafe(method(textDidBeginEditing:))]
        fn did_begin(&self, _n: &NSNotification) {
            self.ivars().push("didBegin".into());
        }

        #[unsafe(method(textDidChange:))]
        fn did_change(&self, _n: &NSNotification) {
            self.ivars().push("didChange".into());
        }

        #[unsafe(method(textShouldEndEditing:))]
        fn should_end(&self, _t: &AnyObject) -> bool {
            self.ivars().push("shouldEnd".into());
            true
        }

        #[unsafe(method(textDidEndEditing:))]
        fn did_end(&self, n: &NSNotification) {
            let key = NSString::from_str("NSTextMovement");
            let movement = n
                .userInfo()
                .and_then(|i| i.objectForKey(&key))
                .and_then(|v| v.downcast::<NSNumber>().ok())
                .map(|v| v.integerValue());
            self.ivars().push(format!("didEnd {movement:?}"));
        }

        #[unsafe(method(textView:doCommandBySelector:))]
        fn do_command(&self, _tv: &NSTextView, sel: Sel) -> bool {
            self.ivars().push(format!("doCommand {}", sel.name().to_str().unwrap()));
            false
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextViewTestEditorDelegate"]
    struct EditorDelegate;

    unsafe impl NSObjectProtocol for EditorDelegate {}

    unsafe impl NSWindowDelegate for EditorDelegate {
        #[unsafe(method_id(windowWillReturnFieldEditor:toObject:))]
        fn window_will_return_field_editor(
            &self,
            _w: &NSWindow,
            object: Option<&AnyObject>,
        ) -> Option<Retained<AnyObject>> {
            let editor =
                object.and_then(|o| o.downcast_ref::<FieldLike>()).and_then(|f| f.ivars().editor.borrow().clone());
            editor.map(|e| {
                Retained::into_super(Retained::into_super(Retained::into_super(Retained::into_super(
                    Retained::into_super(e),
                ))))
            })
        }
    }
);

/// A range as (location, length).
type Span = (usize, usize);
/// Text, insertion point, command, text after, selection after.
type CaseChange = (&'static str, usize, &'static str, &'static str, Span);
/// Command, undo action name, selection after undo, after redo.
type UndoCase = (&'static str, &'static str, Span, Span);
/// Selection, edited range, new text, selection after.
type StorageEdit = (Span, Span, &'static str, Span);
/// How (0 view, 1 storage, 2 attributes), range, new text, text after,
/// selection after, after undo, after redo.
type ProgramEdit = (usize, Span, Option<&'static str>, &'static str, Span, Span, Span);
/// Ranges given, ranges kept.
type RangesCase = (&'static [Span], &'static [Span]);

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

fn mono() -> Retained<NSFont> {
    NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular })
}

/// A TextKit 1 text view: storage, layout manager, container, view.
fn text_view(mtm: MainThreadMarker) -> Retained<NSTextView> {
    let frame = NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 200.0));
    let ts = NSTextStorage::new();
    let lm = NSLayoutManager::new();
    ts.addLayoutManager(&lm);
    let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(300.0, 1.0e7));
    lm.addTextContainer(&tc);
    let tv = NSTextView::initWithFrame_textContainer(NSTextView::alloc(mtm), frame, Some(&tc));
    tv.setFont(Some(&mono()));
    tv
}

fn delegate(mtm: MainThreadMarker, tv: &NSTextView) -> Retained<Delegate> {
    let d: Retained<Delegate> = unsafe { msg_send![super(Delegate::alloc(mtm).set_ivars(Log::default())), init] };
    tv.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    d
}

fn sel_of(tv: &NSTextView) -> (usize, usize) {
    let r = tv.selectedRange();
    (r.location, r.length)
}

fn type_text(tv: &NSTextView, text: &str) {
    let none = NSRange::new(NSNotFound as usize, 0);
    unsafe { NSTextInputClient::insertText_replacementRange(tv, &s(text), none) };
}

fn command(tv: &NSTextView, name: &str) {
    let sel = Sel::register(&std::ffi::CString::new(name).unwrap());
    unsafe { NSTextInputClient::doCommandBySelector(tv, sel) };
}

fn turn() {
    NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.02));
}

/// A text view that undoes, with the manager its delegate gives it.
fn undoing_view(
    mtm: MainThreadMarker,
    text: &str,
) -> (Retained<NSTextView>, Retained<Delegate>, Retained<NSUndoManager>) {
    let tv = text_view(mtm);
    let d = delegate(mtm, &tv);
    let um = NSUndoManager::new(mtm);
    *d.ivars().um.borrow_mut() = Some(um.clone());
    tv.setAllowsUndo(true);
    tv.setString(&s(text));
    (tv, d, um)
}

fn text_of(tv: &NSTextView) -> String {
    tv.string().to_string()
}

fn storage_of(tv: &NSTextView) -> Retained<NSTextStorage> {
    unsafe { tv.textStorage() }.expect("a text storage")
}

fn value(a: usize, b: usize) -> Retained<NSValue> {
    unsafe { NSValue::valueWithRange(NSRange::new(a, b)) }
}

fn ranges_of(tv: &NSTextView) -> Vec<(usize, usize)> {
    tv.selectedRanges()
        .iter()
        .map(|v| {
            let r = unsafe { v.rangeValue() };
            (r.location, r.length)
        })
        .collect()
}

fn defaults(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    assert!(tv.isEditable() && tv.isSelectable() && tv.isRichText());
    assert!(!tv.isFieldEditor() && !tv.allowsUndo() && !tv.importsGraphics());
    assert!(tv.drawsBackground());
    assert_eq!(tv.textContainerInset(), NSSize::ZERO);
    assert_eq!(tv.textContainerOrigin(), NSPoint::ZERO);
    assert!(!tv.isHorizontallyResizable() && !tv.isVerticallyResizable());
    assert_eq!((tv.minSize(), tv.maxSize()), (NSSize::new(300.0, 200.0), NSSize::new(300.0, 200.0)));
    assert_eq!(sel_of(&tv), (0, 0));
    assert!(tv.isFlipped());
    let fresh = NSTextView::initWithFrame_textContainer(
        NSTextView::alloc(mtm),
        NSRect::ZERO,
        Some(&NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(10.0, 10.0))),
    );
    let keys: Vec<String> = fresh.typingAttributes().allKeys().iter().map(|k| k.to_string()).collect::<Vec<_>>();
    assert!(keys.contains(&"NSFont".to_string()) && keys.contains(&"NSColor".to_string()), "{keys:?}");
    assert_eq!(fresh.font().map(|f| f.pointSize()), Some(12.0));
    assert!(unsafe { tv.textStorage() }.is_some() && unsafe { tv.layoutManager() }.is_some());
    unsafe {
        assert_eq!(objc2_app_kit::NSTextDidChangeNotification.to_string(), "NSTextDidChangeNotification");
        assert_eq!(objc2_app_kit::NSTextMovementUserInfoKey.to_string(), "NSTextMovement");
        assert_eq!(
            objc2_app_kit::NSTextViewDidChangeSelectionNotification.to_string(),
            "NSTextViewDidChangeSelectionNotification"
        );
    }
}

/// One user edit: begin editing, ask, change, select, tell; outside a key
/// window, editing ends at once.
fn edit_transaction(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    let d = delegate(mtm, &tv);
    type_text(&tv, "hello");
    assert_eq!(tv.string().to_string(), "hello");
    assert_eq!(sel_of(&tv), (5, 0));
    assert_eq!(
        d.ivars().take(),
        [
            "shouldBegin",
            "didBegin",
            "shouldChange (0, 0) \"hello\"",
            "willChangeSelection (0, 0) (5, 0)",
            "didChangeSelection from Some((0, 0))",
            "didChange",
            "shouldEnd",
            "didEnd"
        ]
    );
    // A move is no edit.
    command(&tv, "moveLeft:");
    assert_eq!(
        d.ivars().take(),
        ["doCommand moveLeft:", "willChangeSelection (5, 0) (4, 0)", "didChangeSelection from Some((5, 0))"]
    );
    // A delegate that handles a command itself: nothing more happens.
    *d.ivars().refuse.borrow_mut() = Some(sel!(moveLeft:));
    command(&tv, "moveLeft:");
    assert_eq!(d.ivars().take(), ["doCommand moveLeft:"]);
    assert_eq!(sel_of(&tv), (4, 0));
    // A refused change changes nothing.
    type_text(&tv, "refused");
    assert_eq!(tv.string().to_string(), "hello");
    // setString: and setSelectedRange: are no user edits.
    d.ivars().take();
    tv.setString(&s("line one\nline two"));
    assert_eq!(sel_of(&tv), (17, 0));
    assert_eq!(d.ivars().take(), ["willChangeSelection (4, 0) (17, 0)", "didChangeSelection from Some((4, 0))"]);
    tv.setSelectedRange(NSRange::new(3, 0));
    assert_eq!(d.ivars().take(), ["willChangeSelection (17, 0) (3, 0)", "didChangeSelection from Some((17, 0))"]);
    // A view that can't be edited takes no typing.
    tv.setEditable(false);
    type_text(&tv, "x");
    assert_eq!(tv.string().to_string(), "line one\nline two");
    assert!(tv.isSelectable());
}

/// The commands, each from a fresh text and insertion point, as macOS does
/// them.
fn commands(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    let text = "one two three\nfour five six\nseven";
    let table: &[(usize, &str, (usize, usize), &str)] = &[
        (5, "moveToBeginningOfLine:", (0, 0), text),
        (5, "moveToEndOfLine:", (13, 0), text),
        (5, "moveToBeginningOfParagraph:", (0, 0), text),
        (5, "moveToEndOfParagraph:", (13, 0), text),
        (5, "moveDown:", (19, 0), text),
        (20, "moveUp:", (6, 0), text),
        (0, "moveUp:", (0, 0), text),
        (30, "moveDown:", (33, 0), text),
        (5, "moveWordRight:", (7, 0), text),
        (4, "moveWordLeft:", (0, 0), text),
        (5, "moveToEndOfDocument:", (33, 0), text),
        (5, "moveBackward:", (4, 0), text),
        (5, "moveForward:", (6, 0), text),
        (13, "moveRight:", (14, 0), text),
        (5, "moveLeftAndModifySelection:", (4, 1), text),
        (5, "moveParagraphForwardAndModifySelection:", (5, 9), text),
        (5, "selectWord:", (4, 3), text),
        (5, "selectLine:", (0, 14), text),
        (5, "selectParagraph:", (0, 14), text),
        (5, "selectAll:", (0, 33), text),
        (5, "deleteWordBackward:", (4, 0), "one wo three\nfour five six\nseven"),
        (5, "deleteToEndOfLine:", (5, 0), "one t\nfour five six\nseven"),
        (5, "deleteToBeginningOfLine:", (0, 0), "wo three\nfour five six\nseven"),
        (5, "deleteForward:", (5, 0), "one to three\nfour five six\nseven"),
        (5, "deleteBackward:", (4, 0), "one wo three\nfour five six\nseven"),
        (5, "transpose:", (6, 0), "one wto three\nfour five six\nseven"),
        (5, "capitalizeWord:", (4, 3), "one Two three\nfour five six\nseven"),
        (5, "uppercaseWord:", (4, 3), "one TWO three\nfour five six\nseven"),
        (5, "insertNewline:", (6, 0), "one t\nwo three\nfour five six\nseven"),
        (5, "insertTab:", (6, 0), "one t\two three\nfour five six\nseven"),
    ];
    for &(start, name, want_sel, want_text) in table {
        tv.setString(&s(text));
        tv.setSelectedRange(NSRange::new(start, 0));
        command(&tv, name);
        assert_eq!((sel_of(&tv), tv.string().to_string().as_str()), (want_sel, want_text), "{name} from {start}");
    }
    // The kill buffer: what deleteToBeginningOfLine: took, yank: puts back.
    tv.setString(&s(text));
    tv.setSelectedRange(NSRange::new(5, 0));
    command(&tv, "deleteToBeginningOfLine:");
    tv.setString(&s(text));
    tv.setSelectedRange(NSRange::new(5, 0));
    command(&tv, "yank:");
    assert_eq!(tv.string().to_string(), "one tone two three\nfour five six\nseven");
    assert_eq!(sel_of(&tv), (10, 0));
    // Extending keeps the far end.
    tv.setSelectedRange(NSRange::new(5, 0));
    command(&tv, "moveRightAndModifySelection:");
    command(&tv, "moveRightAndModifySelection:");
    assert_eq!(sel_of(&tv), (5, 2));
    command(&tv, "moveLeftAndModifySelection:");
    assert_eq!(sel_of(&tv), (5, 1));
    // A selection collapses toward the move.
    command(&tv, "moveRight:");
    assert_eq!(sel_of(&tv), (6, 0));
    // A paragraph separator is U+2029.
    tv.setString(&s("abc"));
    tv.setSelectedRange(NSRange::new(1, 0));
    command(&tv, "insertParagraphSeparator:");
    assert_eq!(text_of(&tv), "a\u{2029}bc");
    // Extending to a line's, paragraph's or the text's end (or start)
    // moves the selection's edge that way; the other edge stays.
    let line = "abcdefgh ijklmnop qrstuv";
    let extend: &[(usize, &str, &str, (usize, usize))] = &[
        (4, "moveWordLeftAndModifySelection:", "moveToEndOfLineAndModifySelection:", (0, 24)),
        (12, "moveWordRightAndModifySelection:", "moveToBeginningOfLineAndModifySelection:", (0, 17)),
        (12, "moveWordLeftAndModifySelection:", "moveToEndOfDocumentAndModifySelection:", (9, 15)),
        (12, "moveWordLeftAndModifySelection:", "moveToEndOfParagraphAndModifySelection:", (9, 15)),
        (12, "moveWordRightAndModifySelection:", "moveToBeginningOfDocumentAndModifySelection:", (0, 17)),
    ];
    for &(at, first, then, want) in extend {
        tv.setString(&s(line));
        tv.setSelectedRange(NSRange::new(at, 0));
        command(&tv, first);
        command(&tv, then);
        assert_eq!(sel_of(&tv), want, "{first} then {then} from {at}");
    }
    // Case changes take the word the insertion point follows.
    let cases: &[CaseChange] = &[
        ("hello world again", 5, "capitalizeWord:", "Hello world again", (0, 5)),
        ("hello world again", 5, "uppercaseWord:", "HELLO world again", (0, 5)),
        ("hello world again", 2, "capitalizeWord:", "Hello world again", (0, 5)),
        ("hello world again", 6, "capitalizeWord:", "hello world again", (5, 1)),
        ("hello  world", 6, "uppercaseWord:", "hello  world", (5, 2)),
        ("hello world again", 17, "uppercaseWord:", "hello world AGAIN", (12, 5)),
    ];
    for &(t, at, name, want, sel) in cases {
        tv.setString(&s(t));
        tv.setSelectedRange(NSRange::new(at, 0));
        command(&tv, name);
        assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), (want, sel), "{name} at {at} in {t:?}");
    }
    // A conjunct is one character to move over, but deleting takes it a
    // consonant at a time; a letter and its accent go together.
    let units = |tv: &NSTextView| text_of(tv).chars().map(|c| c as u32).collect::<Vec<_>>();
    tv.setString(&s("\u{915}\u{94d}\u{937}"));
    tv.setSelectedRange(NSRange::new(3, 0));
    command(&tv, "deleteBackward:");
    assert_eq!((units(&tv), sel_of(&tv)), (vec![0x915, 0x94d], (2, 0)));
    tv.setString(&s("\u{915}\u{94d}\u{937}"));
    tv.setSelectedRange(NSRange::new(0, 0));
    command(&tv, "deleteForward:");
    assert_eq!(units(&tv), [0x937]);
    tv.setString(&s("\u{915}\u{94d}\u{937}a"));
    tv.setSelectedRange(NSRange::new(0, 0));
    command(&tv, "moveRight:");
    assert_eq!(sel_of(&tv), (3, 0));
    tv.setString(&s("e\u{301}x"));
    tv.setSelectedRange(NSRange::new(2, 0));
    command(&tv, "deleteBackward:");
    assert_eq!(text_of(&tv), "x");
    // What a text view doesn't do.
    assert!(!tv.respondsToSelector(sel!(transposeWords:)));
    assert!(!tv.respondsToSelector(sel!(changeCaseOfLetter:)));
}

fn granularity(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    tv.setString(&s("line oneABC\nline two"));
    let at = |loc: usize, g: usize| {
        let r = tv.selectionRangeForProposedRange_granularity(NSRange::new(loc, 0), NSSelectionGranularity(g as _));
        (r.location, r.length)
    };
    assert_eq!(at(2, 1), (0, 4));
    assert_eq!(at(2, 2), (0, 12));
    assert_eq!(at(12, 1), (12, 4));
    assert_eq!(at(14, 2), (12, 8));
    // Points to insertion points: the second character's left half.
    let lm = unsafe { tv.layoutManager() }.unwrap();
    let tc = unsafe { tv.textContainer() }.unwrap();
    let adv = lm.boundingRectForGlyphRange_inTextContainer(NSRange::new(0, 1), &tc).size.width;
    let pad = tc.lineFragmentPadding();
    assert_eq!(tv.characterIndexForInsertionAtPoint(NSPoint::new(pad + 1.25 * adv, 3.0)), 1);
    assert_eq!(tv.characterIndexForInsertionAtPoint(NSPoint::new(pad + 1.75 * adv, 3.0)), 2);
}

/// Typing coalesces into one undo; undo puts the text and selection back,
/// redo does it again.
fn undo(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    let d = delegate(mtm, &tv);
    let um = NSUndoManager::new(mtm);
    *d.ivars().um.borrow_mut() = Some(um.clone());
    tv.setString(&s("line one\nline two"));
    tv.setAllowsUndo(true);
    assert!(tv.undoManager().is_some_and(|u| std::ptr::eq(&*u, &*um)));
    tv.setSelectedRange(NSRange::new(8, 0));
    type_text(&tv, "A");
    type_text(&tv, "B");
    assert!(um.canUndo());
    assert_eq!(um.undoActionName().to_string(), "Typing");
    assert!(tv.isCoalescingUndo());
    turn();
    type_text(&tv, "C");
    turn();
    assert_eq!(tv.string().to_string(), "line oneABC\nline two");
    um.undo();
    assert_eq!(tv.string().to_string(), "line one\nline two");
    assert_eq!(sel_of(&tv), (8, 0));
    assert!(um.canRedo());
    um.redo();
    assert_eq!(tv.string().to_string(), "line oneABC\nline two");
    assert_eq!(sel_of(&tv), (11, 0));
    // A move breaks the run: a new one starts.
    tv.setSelectedRange(NSRange::new(0, 0));
    assert!(!tv.isCoalescingUndo());
    type_text(&tv, ">");
    turn();
    um.undo();
    assert_eq!(tv.string().to_string(), "line oneABC\nline two");
    // Deleting undoes too.
    tv.setSelectedRange(NSRange::new(4, 4));
    command(&tv, "deleteBackward:");
    turn();
    assert_eq!(tv.string().to_string(), "lineABC\nline two");
    um.undo();
    assert_eq!(tv.string().to_string(), "line oneABC\nline two");
    drop(d);
    // Each command's name, and the selection undo and redo leave: undo
    // selects what came back, redo puts the insertion point after it.
    let table: &[UndoCase] = &[
        ("deleteBackward:", "Typing", (4, 1), (4, 0)),
        ("deleteForward:", "", (5, 1), (5, 0)),
        ("deleteWordBackward:", "", (0, 5), (0, 0)),
        ("deleteWordForward:", "", (5, 6), (5, 0)),
        ("deleteToEndOfLine:", "", (5, 6), (5, 0)),
        ("deleteToBeginningOfLine:", "", (0, 5), (0, 0)),
        ("insertNewline:", "Typing", (5, 0), (6, 0)),
        ("insertTab:", "Typing", (5, 0), (6, 0)),
        ("insertParagraphSeparator:", "Typing", (5, 0), (6, 0)),
        ("transpose:", "", (4, 2), (6, 0)),
        ("capitalizeWord:", "", (0, 5), (5, 0)),
        ("insertText", "Typing", (5, 0), (8, 0)),
    ];
    for &(name, action, undone, redone) in table {
        let (tv, _d, um) = undoing_view(mtm, "hello world");
        tv.setSelectedRange(NSRange::new(5, 0));
        if name == "insertText" {
            type_text(&tv, "XYZ");
        } else {
            command(&tv, name);
        }
        let after = text_of(&tv);
        assert_eq!(um.undoActionName().to_string(), action, "{name}");
        turn();
        um.undo();
        assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("hello world", undone), "undoing {name}");
        um.redo();
        assert_eq!((text_of(&tv), sel_of(&tv)), (after, redone), "redoing {name}");
    }
    // Typing replacing a selection, and a pasteboard's text read in
    // (unnamed, unlike paste:).
    let (tv, _d, um) = undoing_view(mtm, "hello world");
    tv.setSelectedRange(NSRange::new(0, 5));
    type_text(&tv, "HEY");
    turn();
    um.undo();
    assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("hello world", (0, 5)));
    um.redo();
    assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("HEY world", (3, 0)));
    let other = NSPasteboard::pasteboardWithUniqueName();
    other.clearContents();
    other.setString_forType(&s("PASTED"), unsafe { NSPasteboardTypeString });
    tv.setSelectedRange(NSRange::new(0, 3));
    assert!(tv.readSelectionFromPasteboard(&other));
    assert_eq!(um.undoActionName().to_string(), "");
    turn();
    um.undo();
    assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("HEY world", (0, 3)));
    um.redo();
    assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("PASTED world", (6, 0)));
    unsafe {
        let _: () = msg_send![&*other, releaseGlobally];
    }
    // One run of typing takes in backward deletions, across turns, even
    // past where it began, and a composition committed after it.
    let (tv, _d, um) = undoing_view(mtm, "abc");
    tv.setSelectedRange(NSRange::new(3, 0));
    type_text(&tv, "x");
    turn();
    command(&tv, "deleteBackward:");
    turn();
    command(&tv, "deleteBackward:");
    turn();
    type_text(&tv, "y");
    turn();
    assert_eq!((text_of(&tv).as_str(), um.undoCount()), ("aby", 1));
    um.undo();
    assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("abc", (2, 1)));
    um.redo();
    assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("aby", (3, 0)));
    let (tv, _d, um) = undoing_view(mtm, "abc");
    tv.setSelectedRange(NSRange::new(3, 0));
    command(&tv, "deleteBackward:");
    turn();
    command(&tv, "deleteBackward:");
    turn();
    type_text(&tv, "y");
    turn();
    assert_eq!((um.undoCount(), um.undoActionName().to_string().as_str()), (1, "Typing"));
    um.undo();
    assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("abc", (1, 2)));
    let (tv, _d, um) = undoing_view(mtm, "abc");
    let none = NSRange::new(NSNotFound as usize, 0);
    tv.setSelectedRange(NSRange::new(3, 0));
    type_text(&tv, "x");
    turn();
    unsafe { tv.setMarkedText_selectedRange_replacementRange(&s("k"), NSRange::new(1, 0), none) };
    turn();
    unsafe { tv.setMarkedText_selectedRange_replacementRange(&s("ka"), NSRange::new(2, 0), none) };
    turn();
    type_text(&tv, "か");
    turn();
    assert_eq!((text_of(&tv).as_str(), um.undoCount()), ("abcxか", 1));
    um.undo();
    assert_eq!(text_of(&tv), "abc");
    // Forward deletions don't run together.
    let (tv, _d, um) = undoing_view(mtm, "abcdef");
    tv.setSelectedRange(NSRange::new(2, 0));
    command(&tv, "deleteForward:");
    turn();
    command(&tv, "deleteForward:");
    turn();
    assert_eq!(um.undoCount(), 2);
    um.undo();
    assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("abdef", (2, 1)));
    // Typing with registration disabled leaves nothing to undo, not even
    // a name; after removeAllActions, typing is undoable again.
    let (tv, _d, um) = undoing_view(mtm, "");
    um.disableUndoRegistration();
    type_text(&tv, "loaded");
    um.enableUndoRegistration();
    turn();
    assert!(!um.canUndo());
    assert_eq!(um.undoActionName().to_string(), "");
    type_text(&tv, "a");
    turn();
    um.removeAllActions();
    type_text(&tv, "b");
    turn();
    assert!(um.canUndo());
    um.undo();
    assert_eq!(text_of(&tv), "loadeda");
}

/// Marked text: shown in the text, replaced by the next update or by the
/// text committed.
fn input_client(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    tv.setString(&s("ab"));
    tv.setSelectedRange(NSRange::new(1, 0));
    let none = NSRange::new(NSNotFound as usize, 0);
    assert!(!tv.hasMarkedText());
    unsafe { tv.setMarkedText_selectedRange_replacementRange(&s("x"), NSRange::new(1, 0), none) };
    assert!(tv.hasMarkedText());
    assert_eq!((tv.markedRange().location, tv.markedRange().length), (1, 1));
    assert_eq!(tv.string().to_string(), "axb");
    assert_eq!(sel_of(&tv), (2, 0));
    unsafe { tv.setMarkedText_selectedRange_replacementRange(&s("xyz"), NSRange::new(1, 1), none) };
    assert_eq!(tv.string().to_string(), "axyzb");
    assert_eq!(sel_of(&tv), (2, 1));
    unsafe { NSTextInputClient::insertText_replacementRange(&*tv, &s("Z"), none) };
    assert!(!tv.hasMarkedText());
    assert_eq!(tv.string().to_string(), "aZb");
    assert_eq!(sel_of(&tv), (2, 0));
    // A replacement range for committed text.
    unsafe { NSTextInputClient::insertText_replacementRange(&*tv, &s("Q"), NSRange::new(0, 1)) };
    assert_eq!(tv.string().to_string(), "QZb");
    let mut actual = NSRange::new(0, 0);
    let sub = unsafe { tv.attributedSubstringForProposedRange_actualRange(NSRange::new(1, 10), &mut actual) };
    assert_eq!(sub.map(|a| a.string().to_string()).as_deref(), Some("Zb"));
    assert_eq!((actual.location, actual.length), (1, 2));
    unsafe { tv.setMarkedText_selectedRange_replacementRange(&s("m"), NSRange::new(1, 0), none) };
    tv.unmarkText();
    assert!(!tv.hasMarkedText());
    assert_eq!(tv.string().to_string(), "QZmb");
    // Where text is on screen: a range's first line piece (without its
    // line break), and the character at a point there.
    let w = test_window(mtm);
    let content = w.contentView().unwrap();
    content.addSubview(&tv);
    tv.setFrame(NSRect::new(NSPoint::new(10.0, 20.0), NSSize::new(300.0, 160.0)));
    tv.setString(&s("hello world\nsecond line"));
    let lm = unsafe { tv.layoutManager() }.unwrap();
    let tc = unsafe { tv.textContainer() }.unwrap();
    let glyph = lm.boundingRectForGlyphRange_inTextContainer(NSRange::new(2, 1), &tc);
    let adv = glyph.size.width;
    let in_window = tv.convertRect_toView(glyph, None);
    let at = w.convertRectToScreen(in_window);
    let mut actual = NSRange::new(0, 0);
    let first = unsafe { tv.firstRectForCharacterRange_actualRange(NSRange::new(2, 15), &mut actual) };
    let close = |a: f64, b: f64| (a - b).abs() < 0.5;
    assert!(close(first.origin.x, at.origin.x) && close(first.origin.y, at.origin.y), "{first:?} vs {at:?}");
    assert!(close(first.size.width, 9.0 * adv) && close(first.size.height, glyph.size.height), "{first:?}");
    assert!(actual.location <= 2 && actual.location + actual.length == 12, "{actual:?}");
    let caret = unsafe { tv.firstRectForCharacterRange_actualRange(NSRange::new(0, 0), &mut actual) };
    assert_eq!(caret.size.width, 0.0);
    let point = NSPoint::new(first.origin.x + 1.0, first.origin.y + first.size.height / 2.0);
    assert_eq!(tv.characterIndexForPoint(point), 2);
    tv.removeFromSuperview();
}

/// A pasteboard's text replaces the selection. (Writing the selection to
/// a pasteboard of one's own answers NO on macOS outside a window; copy:
/// writes to the general pasteboard, which a test shouldn't touch.)
fn pasteboard(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    tv.setString(&s("copy this"));
    tv.setSelectedRange(NSRange::new(5, 4));
    // (What writing answers depends on the platform's pasteboard service
    // in a test process: macOS answers NO here, so it isn't checked.)
    let pb = NSPasteboard::pasteboardWithUniqueName();
    let types = NSArray::from_slice(&[unsafe { NSPasteboardTypeString }]);
    let _ = tv.writeSelectionToPasteboard_types(&pb, &types);
    unsafe {
        let _: () = msg_send![&*pb, releaseGlobally];
    }
    let other = NSPasteboard::pasteboardWithUniqueName();
    other.clearContents();
    other.setString_forType(&s("THAT"), unsafe { NSPasteboardTypeString });
    tv.setSelectedRange(NSRange::new(0, 4));
    assert!(tv.readSelectionFromPasteboard(&other));
    assert_eq!(tv.string().to_string(), "THAT this");
    assert_eq!(sel_of(&tv), (4, 0));
    unsafe {
        let _: () = msg_send![&*other, releaseGlobally];
    }
}

/// A window has its delegate's undo manager, or one of its own, which it
/// keeps once made; views in it find it up the responder chain.
fn window_undo(mtm: MainThreadMarker) {
    let frame = NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(300.0, 200.0));
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    let own = w.undoManager().expect("a window's own undo manager");
    assert!(w.undoManager().is_some_and(|u| std::ptr::eq(&*u, &*own)), "the same one each time");
    let content = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
    w.setContentView(Some(&content));
    let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
    content.addSubview(&view);
    assert!(view.undoManager().is_some_and(|u| std::ptr::eq(&*u, &*own)));
    let d: Retained<WindowDelegate> =
        unsafe { msg_send![super(WindowDelegate::alloc(mtm).set_ivars(Log::default())), init] };
    let theirs = NSUndoManager::new(mtm);
    *d.ivars().um.borrow_mut() = Some(theirs.clone());
    w.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    // Once the window has made its own, it keeps it.
    assert!(w.undoManager().is_some_and(|u| std::ptr::eq(&*u, &*own)));
    w.setDelegate(None);
    let w2 = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { w2.setReleasedWhenClosed(false) };
    w2.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    // Until then the delegate is asked each time.
    assert!(w2.undoManager().is_some_and(|u| std::ptr::eq(&*u, &*theirs)));
    let other = NSUndoManager::new(mtm);
    *d.ivars().um.borrow_mut() = Some(other.clone());
    assert!(w2.undoManager().is_some_and(|u| std::ptr::eq(&*u, &*other)));
    // A delegate with none: the window makes its own.
    *d.ivars().um.borrow_mut() = None;
    assert!(w2.undoManager().is_some());
    w2.setDelegate(None);
}

/// A program's `setString:` leaves the scroll position alone; typing
/// scrolls the insertion point into view.
fn scrolling(mtm: MainThreadMarker) {
    let scroll = NSTextView::scrollableTextView(mtm);
    let tv: Retained<NSTextView> = scroll.documentView().unwrap().downcast().unwrap();
    let sizable = NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable;
    assert_eq!(tv.autoresizingMask(), sizable);
    assert!(!tv.isRichText() && tv.isVerticallyResizable() && !tv.isHorizontallyResizable());
    let doc = NSTextView::scrollableDocumentContentTextView(mtm);
    let dtv: Retained<NSTextView> = doc.documentView().unwrap().downcast().unwrap();
    assert!(dtv.isRichText() && dtv.autoresizingMask() == sizable);
    let plain = NSTextView::scrollablePlainDocumentContentTextView(mtm);
    let ptv: Retained<NSTextView> = plain.documentView().unwrap().downcast().unwrap();
    assert!(!ptv.isRichText());
    // The view fills what shows of it, following the scroll view's size.
    for size in [NSSize::new(300.0, 200.0), NSSize::new(400.0, 500.0), NSSize::new(250.0, 150.0)] {
        scroll.setFrame(NSRect::new(NSPoint::ZERO, size));
        assert_eq!(tv.frame().size, scroll.contentSize(), "in {size:?}");
        assert_eq!(tv.minSize(), scroll.contentSize());
    }
    tv.setString(&s("some text"));
    tv.setString(&s(""));
    assert_eq!(tv.frame().size, scroll.contentSize(), "no shorter once the text goes");
    scroll.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 200.0)));
    tv.setFont(Some(&mono()));
    let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
    tv.setString(&s(&text));
    assert_eq!(sel_of(&tv), (text.len(), 0));
    let clip = scroll.contentView();
    assert_eq!(clip.bounds().origin.y, 0.0, "setString: doesn't scroll");
    type_text(&tv, "x");
    let visible = clip.documentVisibleRect();
    let lm = unsafe { tv.layoutManager() }.unwrap();
    let tc = unsafe { tv.textContainer() }.unwrap();
    let caret = lm.boundingRectForGlyphRange_inTextContainer(NSRange::new(text.len(), 1), &tc);
    assert!(visible.origin.y > 0.0, "typing scrolls");
    assert!(
        caret.origin.y >= visible.origin.y
            && caret.origin.y + caret.size.height <= visible.origin.y + visible.size.height,
        "to the insertion point"
    );
}

fn test_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let frame = NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(300.0, 200.0));
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    let content = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, frame.size));
    w.setContentView(Some(&content));
    w
}

/// The window's field editor, and how it ends editing: Return, Tab and
/// Backtab tell the delegate with the movement and put nothing in the
/// text; Escape is only offered as a command.
fn field_editor(mtm: MainThreadMarker) {
    let w = test_window(mtm);
    let control: Retained<FieldLike> = unsafe {
        msg_send![super(FieldLike::alloc(mtm).set_ivars(Log::default())), initWithFrame: NSRect::new(NSPoint::new(10.0, 10.0), NSSize::new(200.0, 22.0))]
    };
    w.contentView().unwrap().addSubview(&control);
    let editor = unsafe { w.fieldEditor_forObject(true, Some(&control)) }.expect("a field editor");
    let again = unsafe { w.fieldEditor_forObject(true, None) }.expect("the same one");
    assert!(std::ptr::eq(&*editor, &*again));
    assert!(editor.isFieldEditor());
    assert!(unsafe { w.fieldEditor_forObject(false, None) }.is_some());
    let tv: Retained<NSTextView> = editor.downcast().expect("a text view");
    tv.setString(&s("hello"));
    tv.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(200.0, 22.0)));
    control.addSubview(&tv);
    unsafe {
        let _: () = msg_send![&*tv, setDelegate: &*control];
    }
    assert!(w.makeFirstResponder(Some(&tv)));
    tv.setSelectedRange(NSRange::new(5, 0));
    type_text(&tv, "!");
    assert_eq!(control.ivars().take(), ["shouldBegin", "didBegin", "didChange"]);
    command(&tv, "insertNewline:");
    assert_eq!(control.ivars().take(), ["doCommand insertNewline:", "shouldEnd", "didEnd Some(16)"]);
    assert_eq!(tv.string().to_string(), "hello!");
    command(&tv, "insertTab:");
    // Nothing edited since: nothing to ask.
    assert_eq!(control.ivars().take(), ["doCommand insertTab:", "didEnd Some(17)"]);
    command(&tv, "insertBacktab:");
    assert_eq!(control.ivars().take(), ["doCommand insertBacktab:", "didEnd Some(18)"]);
    command(&tv, "cancelOperation:");
    assert_eq!(control.ivars().take(), ["doCommand cancelOperation:"]);
    assert_eq!(tv.string().to_string(), "hello!");
    // insertNewlineIgnoringFieldEditor: puts a line break in anyway.
    command(&tv, "insertNewlineIgnoringFieldEditor:");
    assert_eq!(tv.string().to_string(), "hello!\n");
    // The window's delegate may hand out an editor of its own.
    let own = NSTextView::initWithFrame_textContainer(
        NSTextView::alloc(mtm),
        NSRect::ZERO,
        Some(&NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(10.0, 10.0))),
    );
    *control.ivars().editor.borrow_mut() = Some(own.clone());
    let d: Retained<EditorDelegate> = unsafe { msg_send![super(EditorDelegate::alloc(mtm).set_ivars(())), init] };
    w.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    let theirs = unsafe { w.fieldEditor_forObject(true, Some(&control)) }.expect("the delegate's editor");
    assert!(std::ptr::eq(Retained::as_ptr(&theirs).cast::<u8>(), Retained::as_ptr(&own).cast::<u8>()));
    w.setDelegate(None);
    // endEditingFor: takes first responder from the field editor.
    unsafe { w.endEditingFor(Some(&control)) };
    let first = w.firstResponder().expect("a first responder");
    assert!(std::ptr::eq(Retained::as_ptr(&first).cast::<u8>(), Retained::as_ptr(&w).cast::<u8>()));
}

/// A program editing the text storage itself (not through the view) moves
/// the view's selection as AppKit does: along with the text after the
/// edit, to the edit's end where they overlap; the marked text is
/// forgotten; typing afterwards goes where the selection now is.
fn storage_edits(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    let d = delegate(mtm, &tv);
    let ts = storage_of(&tv);
    let m: &NSMutableAttributedString = &ts;
    let text = "a long line of text here";
    tv.setString(&s(text));
    tv.setSelectedRange(NSRange::new(20, 3));
    d.ivars().take();
    m.setAttributedString(&NSAttributedString::from_nsstring(&s("hi")));
    assert_eq!(sel_of(&tv), (2, 0));
    assert_eq!(d.ivars().take(), ["willChangeSelection (20, 3) (2, 0)", "didChangeSelection from Some((20, 3))"]);
    type_text(&tv, "!");
    assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("hi!", (3, 0)));
    command(&tv, "deleteBackward:");
    assert_eq!(text_of(&tv), "hi");
    let cases: &[StorageEdit] = &[
        ((8, 2), (0, 5), "HI", (5, 2)),
        ((8, 2), (6, 4), "XY", (8, 0)),
        ((8, 2), (9, 4), "XY", (11, 0)),
        ((8, 2), (7, 5), "", (7, 0)),
        ((8, 0), (8, 0), "ZZ", (10, 0)),
        ((8, 0), (4, 4), "Q", (5, 0)),
        ((8, 2), (10, 2), "W", (8, 2)),
        ((8, 2), (8, 2), "W", (9, 0)),
        ((8, 4), (9, 1), "W", (10, 0)),
    ];
    for &(sel, edit, with, want) in cases {
        tv.setString(&s(text));
        tv.setSelectedRange(NSRange::new(sel.0, sel.1));
        d.ivars().take();
        m.replaceCharactersInRange_withString(NSRange::new(edit.0, edit.1), &s(with));
        assert_eq!(sel_of(&tv), want, "selection {sel:?}, edit {edit:?} to {with:?}");
        let events = d.ivars().take();
        assert_eq!(events.is_empty(), sel == want, "{events:?}");
    }
    // Attributes alone leave it.
    tv.setString(&s(text));
    tv.setSelectedRange(NSRange::new(8, 2));
    let red = objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    unsafe { m.addAttribute_value_range(objc2_app_kit::NSForegroundColorAttributeName, &red, NSRange::new(0, 20)) };
    assert_eq!(sel_of(&tv), (8, 2));
    // Several ranges become the first, moved.
    tv.setString(&s("abcdefghij"));
    tv.setSelectedRanges(&NSArray::from_retained_slice(&[value(1, 2), value(6, 2)]));
    m.replaceCharactersInRange_withString(NSRange::new(0, 1), &s("XYZ"));
    assert_eq!(ranges_of(&tv), [(3, 2)]);
    // Marked text is forgotten by a program's change to the text: through
    // the storage, the view, or setString:, which typing then follows.
    let none = NSRange::new(NSNotFound as usize, 0);
    for how in 0..3 {
        tv.setString(&s("hello world"));
        tv.setSelectedRange(NSRange::new(11, 0));
        unsafe { tv.setMarkedText_selectedRange_replacementRange(&s("abc"), NSRange::new(3, 0), none) };
        assert!(tv.hasMarkedText());
        match how {
            0 => m.replaceCharactersInRange_withString(NSRange::new(0, 5), &s("HI")),
            1 => tv.replaceCharactersInRange_withString(NSRange::new(0, 5), &s("HI")),
            _ => tv.setString(&s("")),
        }
        assert!(!tv.hasMarkedText(), "change {how}");
    }
    type_text(&tv, "x");
    assert_eq!(text_of(&tv), "x");
}

/// A program's own edit, between shouldChangeTextInRange:replacementString:
/// and didChangeText, is undoable, unnamed: through the view, through the
/// storage, or of attributes alone.
fn programmatic_undo(mtm: MainThreadMarker) {
    let red = objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    let cases: &[ProgramEdit] = &[
        // (how, range, string, text after, selection after, after undo, after redo)
        (0, (0, 1), Some("X"), "Xbc", (1, 0), (0, 1), (1, 0)),
        (1, (0, 1), Some("X"), "Xbc", (1, 0), (0, 1), (1, 0)),
        (1, (1, 0), Some("XY"), "aXYbc", (0, 1), (1, 0), (3, 0)),
        (2, (0, 2), None, "abc", (0, 1), (0, 2), (0, 2)),
    ];
    for &(how, range, with, after, sel_after, undone, redone) in cases {
        let (tv, _d, um) = undoing_view(mtm, "abc");
        tv.setSelectedRange(NSRange::new(0, 1));
        let ts = storage_of(&tv);
        let m: &NSMutableAttributedString = &ts;
        let r = NSRange::new(range.0, range.1);
        assert!(tv.shouldChangeTextInRange_replacementString(r, with.map(s).as_deref()));
        match (how, with) {
            (0, Some(w)) => tv.replaceCharactersInRange_withString(r, &s(w)),
            (1, Some(w)) => m.replaceCharactersInRange_withString(r, &s(w)),
            _ => unsafe { m.addAttribute_value_range(objc2_app_kit::NSForegroundColorAttributeName, &red, r) },
        }
        tv.didChangeText();
        assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), (after, sel_after), "case {how} {range:?}");
        assert!(um.canUndo());
        assert_eq!(um.undoActionName().to_string(), "");
        turn();
        assert_eq!(um.undoCount(), 1);
        um.undo();
        assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("abc", undone), "undoing {how} {range:?}");
        um.redo();
        assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), (after, redone), "redoing {how} {range:?}");
    }
}

/// setSelectedRanges: keeps them in order, inside the text, overlapping
/// or touching ones merged, empty ones left out when others aren't; the
/// first is the selection typing replaces.
fn selected_ranges(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    tv.setString(&s("abcdefghij"));
    let cases: &[RangesCase] = &[
        (&[(5, 2), (1, 2), (2, 2)], &[(1, 3), (5, 2)]),
        (&[(5, 0), (1, 2)], &[(1, 2)]),
        (&[(5, 0), (1, 0)], &[(1, 0)]),
        (&[(3, 2), (5, 2)], &[(3, 4)]),
        (&[(8, 10)], &[(8, 2)]),
        (&[(20, 2), (1, 1)], &[(1, 1)]),
    ];
    for &(given, want) in cases {
        let values: Vec<Retained<NSValue>> = given.iter().map(|&(a, b)| value(a, b)).collect();
        tv.setSelectedRanges(&NSArray::from_retained_slice(&values));
        assert_eq!(ranges_of(&tv), want, "{given:?}");
        assert_eq!(sel_of(&tv), want[0]);
    }
    tv.setSelectedRanges(&NSArray::from_retained_slice(&[value(5, 2), value(1, 2), value(2, 2)]));
    type_text(&tv, "Z");
    assert_eq!((text_of(&tv).as_str(), sel_of(&tv)), ("aZefghij", (2, 0)));
}

/// A text view's string is its storage's, live.
fn live_string(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    tv.setString(&s("abc"));
    let string = tv.string();
    type_text(&tv, "d");
    assert_eq!(string.length(), 4);
    assert_eq!(string.to_string(), "abcd");
}

/// initWithFrame: builds a network whose view grows down with its text;
/// initWithFrame:textContainer: keeps the frame it is given.
fn made_with_a_frame(mtm: MainThreadMarker) {
    let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(100.0, 50.0)));
    assert!(tv.isVerticallyResizable() && !tv.isHorizontallyResizable());
    assert_eq!((tv.minSize(), tv.maxSize()), (NSSize::new(100.0, 50.0), NSSize::new(100.0, 1.0e7)));
    let tc = unsafe { tv.textContainer() }.expect("a container");
    assert_eq!(tc.size(), NSSize::new(100.0, 1.0e7));
    assert!(tc.widthTracksTextView());
    let _ = unsafe { tv.layoutManager() };
    tv.setFont(Some(&mono()));
    let text: String = (0..20).map(|i| format!("l{i}\n")).collect();
    type_text(&tv, &text);
    let h = tv.frame().size.height;
    assert!(h > 20.0 * 12.0, "grown to the text: {h}");
    assert_eq!(tv.frame().size.width, 100.0);
    let given = text_view(mtm);
    assert!(!given.isVerticallyResizable());
}

/// A layout manager given another text storage: the view shows and edits
/// it.
fn replaced_storage(mtm: MainThreadMarker) {
    let tv = text_view(mtm);
    tv.setString(&s("old text here"));
    let lm = unsafe { tv.layoutManager() }.unwrap();
    let fresh = NSTextStorage::new();
    let m: &NSMutableAttributedString = &fresh;
    m.replaceCharactersInRange_withString(NSRange::new(0, 0), &s("new"));
    lm.replaceTextStorage(&fresh);
    assert!(unsafe { tv.textStorage() }.is_some_and(|t| std::ptr::eq(&*t, &*fresh)));
    assert_eq!(text_of(&tv), "new");
    tv.setSelectedRange(NSRange::new(3, 0));
    type_text(&tv, "!");
    assert_eq!(fresh.string().to_string(), "new!");
}

/// A text storage and layout manager made on the main thread may be
/// handed to another (they work on any thread, one at a time): the main
/// thread's run loop turning meanwhile leaves them alone.
fn layout_on_another_thread(_mtm: MainThreadMarker) {
    /// The storage is kept alive with its layout manager and container.
    struct Network(#[allow(dead_code)] Retained<NSTextStorage>, Retained<NSLayoutManager>, Retained<NSTextContainer>);
    // SAFETY: handed over whole, and used by one thread at a time.
    unsafe impl Send for Network {}
    let text: String = (0..40_000).map(|i| format!("line {i} of the text\n")).collect();
    let ts = NSTextStorage::new();
    let m: &NSMutableAttributedString = &ts;
    m.replaceCharactersInRange_withString(NSRange::new(0, 0), &s(&text));
    let lm = NSLayoutManager::new();
    ts.addLayoutManager(&lm);
    let tc = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(300.0, 1.0e7));
    lm.addTextContainer(&tc);
    // An edit on the main thread first, as a program loading text does.
    m.replaceCharactersInRange_withString(NSRange::new(0, 0), &s("first\n"));
    let net = Network(ts, lm, tc);
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    let worker = std::thread::spawn(move || {
        let n = net;
        n.1.ensureLayoutForTextContainer(&n.2);
        let height = n.1.usedRectForTextContainer(&n.2).size.height;
        flag.store(true, std::sync::atomic::Ordering::Release);
        (height, n)
    });
    while !done.load(std::sync::atomic::Ordering::Acquire) {
        turn();
    }
    let (height, _net) = worker.join().expect("the worker");
    assert!(height > 40_000.0 * 10.0, "{height}");
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("the test's main runs on the main thread");
    let tests: &[Test] = &[
        ("defaults", defaults),
        ("edit_transaction", edit_transaction),
        ("commands", commands),
        ("granularity", granularity),
        ("undo", undo),
        ("input_client", input_client),
        ("pasteboard", pasteboard),
        ("window_undo", window_undo),
        ("field_editor", field_editor),
        ("scrolling", scrolling),
        ("storage_edits", storage_edits),
        ("programmatic_undo", programmatic_undo),
        ("selected_ranges", selected_ranges),
        ("live_string", live_string),
        ("made_with_a_frame", made_with_a_frame),
        ("replaced_storage", replaced_storage),
        ("layout_on_another_thread", layout_on_another_thread),
    ];
    for (name, test) in tests {
        test(mtm);
        println!("test {name} ... ok");
    }
}
