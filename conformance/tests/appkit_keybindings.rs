//! Key bindings: what `-[NSResponder interpretKeyEvents:]` makes of keys,
//! typed text for `insertText:` and editing commands for
//! `doCommandBySelector:`, and how both travel up the responder chain.
//! Checked on macOS and on Linux alike.
//!
//! The array of events is made through the runtime, so this file links
//! where `NSArray` isn't implemented yet; there the bindings go unchecked.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSEvent, NSEventModifierFlags as F, NSEventType, NSResponder, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

thread_local!(static SEEN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn seen(what: String) {
    SEEN.with(|s| s.borrow_mut().push(what));
}

fn take() -> Vec<String> {
    SEEN.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

define_class!(
    /// Records the text and commands the key bindings send.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceKeyBindingView"]
    struct Editor;

    impl Editor {
        #[unsafe(method(insertText:))]
        fn insert_text(&self, text: &AnyObject) {
            let text: Retained<NSString> = unsafe { msg_send![text, description] };
            seen(format!("insert {text}"));
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            seen(selector.name().to_str().unwrap_or("?").to_string());
        }
    }
);

define_class!(
    /// Performs one editing command, and takes text, for views inside it.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceKeyBindingParent"]
    struct Parent;

    impl Parent {
        #[unsafe(method(moveLeft:))]
        fn move_left(&self, _sender: Option<&AnyObject>) {
            seen("parent moveLeft:".into());
        }

        #[unsafe(method(insertText:))]
        fn insert_text(&self, text: &AnyObject) {
            let text: Retained<NSString> = unsafe { msg_send![text, description] };
            seen(format!("parent insert {text}"));
        }
    }
);

fn frame() -> NSRect {
    NSRect::new(NSPoint::ZERO, NSSize::new(100.0, 100.0))
}

fn key(characters: &str, flags: F) -> Retained<NSEvent> {
    // Control characters are typed with Control: the key is the letter.
    let unmodified = match characters.chars().next() {
        Some(c @ '\u{1}'..='\u{1a}') if flags.contains(F::Control) => ((c as u8 - 1 + b'a') as char).to_string(),
        _ => characters.to_string(),
    };
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::ZERO,
        flags,
        0.0,
        0,
        None,
        &NSString::from_str(characters),
        &NSString::from_str(&unmodified),
        false,
        0,
    )
    .expect("a key event")
}

fn commands_travel_up_the_chain(mtm: MainThreadMarker) {
    let parent: Retained<Parent> =
        unsafe { msg_send![super(Parent::alloc(mtm).set_ivars(())), initWithFrame: frame()] };
    let child = NSView::initWithFrame(NSView::alloc(mtm), frame());
    parent.addSubview(&child);
    take();
    // A command the child doesn't have goes to the parent, which has it.
    let _: () = unsafe { msg_send![&*child, doCommandBySelector: sel!(moveLeft:)] };
    assert_eq!(take(), ["parent moveLeft:"]);
    // One nobody has goes nowhere.
    let _: () = unsafe { msg_send![&*child, doCommandBySelector: sel!(moveRight:)] };
    assert_eq!(take(), Vec::<String>::new());
    // Text goes up to whoever takes it.
    let text = NSString::from_str("hi");
    let _: () = unsafe { msg_send![&*child, insertText: &*text] };
    assert_eq!(take(), ["parent insert hi"]);
}

fn key_bindings(mtm: MainThreadMarker) {
    let Some(array_class) = AnyClass::get(c"NSArray") else {
        println!("(no NSArray yet: key bindings not checked)");
        return;
    };
    let editor: Retained<Editor> =
        unsafe { msg_send![super(Editor::alloc(mtm).set_ivars(())), initWithFrame: frame()] };
    let arrow = F::Function | F::NumericPad;
    let function = F::Function;
    let cases: &[(&str, F, &[&str])] = &[
        ("a", F(0), &["insert a"]),
        ("A", F::Shift, &["insert A"]),
        ("é", F(0), &["insert é"]),
        ("a", F::Control | F::Option, &["insert a"]),
        ("a", F::Command, &["noop:"]),
        ("\u{F704}", function, &["noop:"]),
        ("\r", F(0), &["insertNewline:"]),
        ("\u{3}", F::NumericPad, &["insertNewline:"]),
        ("\t", F(0), &["insertTab:"]),
        ("\u{19}", F::Shift, &["insertBacktab:"]),
        ("\u{1b}", F(0), &["cancelOperation:"]),
        ("\u{7f}", F(0), &["deleteBackward:"]),
        ("\u{7f}", F::Option, &["deleteWordBackward:"]),
        ("\u{7f}", F::Command, &["deleteToBeginningOfLine:"]),
        ("\u{7f}", F::Control, &["deleteBackwardByDecomposingPreviousCharacter:"]),
        ("\u{F728}", function, &["deleteForward:"]),
        ("\u{F728}", function | F::Option, &["deleteWordForward:"]),
        ("\u{F702}", arrow, &["moveLeft:"]),
        ("\u{F703}", arrow, &["moveRight:"]),
        ("\u{F700}", arrow, &["moveUp:"]),
        ("\u{F701}", arrow, &["moveDown:"]),
        ("\u{F702}", arrow | F::Shift, &["moveLeftAndModifySelection:"]),
        ("\u{F701}", arrow | F::Shift, &["moveDownAndModifySelection:"]),
        ("\u{F702}", arrow | F::Option, &["moveWordLeft:"]),
        ("\u{F703}", arrow | F::Option | F::Shift, &["moveWordRightAndModifySelection:"]),
        ("\u{F700}", arrow | F::Option, &["moveBackward:", "moveToBeginningOfParagraph:"]),
        ("\u{F701}", arrow | F::Option, &["moveForward:", "moveToEndOfParagraph:"]),
        ("\u{F702}", arrow | F::Command, &["moveToLeftEndOfLine:"]),
        ("\u{F703}", arrow | F::Command, &["moveToRightEndOfLine:"]),
        ("\u{F700}", arrow | F::Command, &["moveToBeginningOfDocument:"]),
        ("\u{F701}", arrow | F::Command | F::Shift, &["moveToEndOfDocumentAndModifySelection:"]),
        ("\u{F703}", arrow | F::Control, &["moveToRightEndOfLine:"]),
        ("\u{F729}", function, &["scrollToBeginningOfDocument:"]),
        ("\u{F72B}", function, &["scrollToEndOfDocument:"]),
        ("\u{F72C}", function, &["scrollPageUp:"]),
        ("\u{F72D}", function, &["scrollPageDown:"]),
        ("\u{F729}", function | F::Shift, &["moveToBeginningOfDocumentAndModifySelection:"]),
        ("\u{F72D}", function | F::Option, &["pageDown:"]),
        ("\u{1}", F::Control, &["moveToBeginningOfParagraph:"]),
        ("\u{1}", F::Control | F::Shift, &["moveToBeginningOfParagraph:"]),
        ("\u{2}", F::Control, &["moveBackward:"]),
        ("\u{4}", F::Control, &["deleteForward:"]),
        ("\u{5}", F::Control, &["moveToEndOfParagraph:"]),
        ("\u{6}", F::Control, &["moveForward:"]),
        ("\u{8}", F::Control, &["deleteBackward:"]),
        ("\u{b}", F::Control, &["deleteToEndOfParagraph:"]),
        ("\u{c}", F::Control, &["centerSelectionInVisibleArea:"]),
        ("\u{e}", F::Control, &["moveDown:"]),
        ("\u{f}", F::Control, &["insertNewlineIgnoringFieldEditor:", "moveBackward:"]),
        ("\u{10}", F::Control, &["moveUp:"]),
        ("\u{14}", F::Control, &["transpose:"]),
        ("\u{16}", F::Control, &["pageDown:"]),
        ("\u{19}", F::Control, &["yank:"]),
    ];
    for (chars, flags, expected) in cases {
        let event = key(chars, *flags);
        let array: Retained<AnyObject> = unsafe { msg_send![array_class, arrayWithObject: &*event] };
        let _: () = unsafe { msg_send![&*editor, interpretKeyEvents: &*array] };
        assert_eq!(take(), *expected, "{chars:?} with {:#x}", flags.0);
    }
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] =
        &[("commands_travel_up_the_chain", commands_travel_up_the_chain), ("key_bindings", key_bindings)];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
