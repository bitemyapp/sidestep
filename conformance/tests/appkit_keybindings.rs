//! Key bindings: what `-[NSResponder interpretKeyEvents:]` makes of keys,
//! typed text for `insertText:` and editing commands for
//! `doCommandBySelector:`, and how both travel up the responder chain.
//! Checked on macOS and on Linux alike, against the table in
//! keybindings.tsv (recorded from AppKit): every key of a US keyboard with
//! every combination of modifiers.
//!
//! The table goes through `-[NSTextInputContext handleEvent:]`, which needs
//! no `NSArray`, and through `interpretKeyEvents:` where `NSArray` exists;
//! its array is made through the runtime, so this file links where it
//! isn't implemented yet, and that check reports itself skipped there.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSEvent, NSEventModifierFlags as F, NSEventType, NSResponder,
    NSTextInputClient, NSView,
};
use objc2_foundation::{NSAttributedString, NSPoint, NSRange, NSRect, NSSize, NSString};

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
            seen(format!("insert {}", hex_of(text)));
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            seen(selector.name().to_str().unwrap_or("?").to_string());
        }
    }
);

/// Text as the table writes it: code points in hex, joined by `+`.
fn hex_of(text: &AnyObject) -> String {
    let text: Retained<NSString> = unsafe { msg_send![text, description] };
    text.to_string().chars().map(|c| format!("{:x}", c as u32)).collect::<Vec<_>>().join("+")
}

define_class!(
    /// Performs one editing command, and takes text, for views inside it.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceKeyBindingParent"]
    struct Parent;

    impl Parent {
        #[unsafe(method(moveLeft:))]
        fn move_left(&self, sender: Option<&AnyObject>) {
            seen(format!("parent moveLeft: from {}", if sender.is_some() { "a sender" } else { "nil" }));
        }

        #[unsafe(method(insertText:))]
        fn insert_text(&self, text: &AnyObject) {
            let text: Retained<NSString> = unsafe { msg_send![text, description] };
            seen(format!("parent insert {text}"));
        }
    }
);

define_class!(
    /// Takes actions, and stands in as the application's delegate.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceActionTarget"]
    struct ActionTarget;

    impl ActionTarget {
        #[unsafe(method(copy:))]
        fn copy_action(&self, sender: Option<&AnyObject>) {
            seen(format!("target copy: from {}", if sender.is_some() { "a sender" } else { "nil" }));
        }

        #[unsafe(method(conformanceDelegateAction:))]
        fn delegate_action(&self, _sender: Option<&AnyObject>) {
            seen("delegate action".into());
        }
    }

    unsafe impl NSObjectProtocol for ActionTarget {}
    unsafe impl NSApplicationDelegate for ActionTarget {}
);

/// `NSNotFound`.
const NOT_FOUND: usize = isize::MAX as usize;

define_class!(
    /// Takes text the way input methods give it, and records it.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceTextInputClient"]
    struct TextClient;

    unsafe impl NSObjectProtocol for TextClient {}

    unsafe impl NSTextInputClient for TextClient {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text_replacement_range(&self, text: &AnyObject, range: NSRange) {
            let text: Retained<NSString> = unsafe { msg_send![text, description] };
            let at = if range.location == NOT_FOUND { "none".to_string() } else { range.location.to_string() };
            seen(format!("insert {text} at {at}"));
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            seen(selector.name().to_str().unwrap_or("?").to_string());
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, _text: &AnyObject, _selected: NSRange, _replaced: NSRange) {}

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {}

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(0, 0)
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            NSRange::new(NOT_FOUND, 0)
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            false
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(&self, _range: NSRange, _actual: *mut NSRange) -> Option<Retained<NSAttributedString>> {
            None
        }

        // An empty array, made through the runtime like the one above.
        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Option<Retained<AnyObject>> {
            match AnyClass::get(c"NSArray") {
                Some(class) => unsafe { msg_send![class, array] },
                None => None,
            }
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect(&self, _range: NSRange, _actual: *mut NSRange) -> NSRect {
            NSRect::ZERO
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> usize {
            0
        }
    }
);

define_class!(
    /// Takes the table's keys through an input context, and records them
    /// as the table writes them.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceKeyTableClient"]
    struct TableClient;

    unsafe impl NSObjectProtocol for TableClient {}

    unsafe impl NSTextInputClient for TableClient {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text_replacement_range(&self, text: &AnyObject, _range: NSRange) {
            seen(format!("insert {}", hex_of(text)));
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            seen(selector.name().to_str().unwrap_or("?").to_string());
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, text: &AnyObject, _selected: NSRange, _replaced: NSRange) {
            seen(format!("mark {}", hex_of(text)));
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {}

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(0, 0)
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            NSRange::new(NOT_FOUND, 0)
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            false
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(&self, _range: NSRange, _actual: *mut NSRange) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Option<Retained<AnyObject>> {
            match AnyClass::get(c"NSArray") {
                Some(class) => unsafe { msg_send![class, array] },
                None => None,
            }
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect(&self, _range: NSRange, _actual: *mut NSRange) -> NSRect {
            NSRect::ZERO
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> usize {
            0
        }
    }
);

fn frame() -> NSRect {
    NSRect::new(NSPoint::ZERO, NSSize::new(100.0, 100.0))
}

/// A key event as a keyboard makes it: Control characters are typed with
/// Control, the key being the letter, in upper case with Shift.
fn key(characters: &str, flags: F) -> Retained<NSEvent> {
    let unmodified = match characters.chars().next() {
        Some(c @ '\u{1}'..='\u{1a}') if flags.contains(F::Control) => {
            let letter = (c as u8 - 1 + b'a') as char;
            if flags.contains(F::Shift) { letter.to_ascii_uppercase() } else { letter }.to_string()
        }
        _ => characters.to_string(),
    };
    event(characters, &unmodified, flags)
}

fn event(characters: &str, unmodified: &str, flags: F) -> Retained<NSEvent> {
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::ZERO,
        flags,
        0.0,
        0,
        None,
        &NSString::from_str(characters),
        &NSString::from_str(unmodified),
        false,
        0,
    )
    .expect("a key event")
}

fn commands_travel_up_the_chain(mtm: MainThreadMarker) -> Outcome {
    let parent: Retained<Parent> =
        unsafe { msg_send![super(Parent::alloc(mtm).set_ivars(())), initWithFrame: frame()] };
    let child = NSView::initWithFrame(NSView::alloc(mtm), frame());
    parent.addSubview(&child);
    take();
    // A command the child doesn't have goes to the parent, which has it,
    // with no sender.
    let _: () = unsafe { msg_send![&*child, doCommandBySelector: sel!(moveLeft:)] };
    assert_eq!(take(), ["parent moveLeft: from nil"]);
    let _: () = unsafe { msg_send![&*parent, doCommandBySelector: sel!(moveLeft:)] };
    assert_eq!(take(), ["parent moveLeft: from nil"]);
    // One nobody has goes nowhere.
    let _: () = unsafe { msg_send![&*child, doCommandBySelector: sel!(moveRight:)] };
    assert_eq!(take(), Vec::<String>::new());
    // Text goes up to whoever takes it.
    let text = NSString::from_str("hi");
    let _: () = unsafe { msg_send![&*child, insertText: &*text] };
    assert_eq!(take(), ["parent insert hi"]);
    Ok(())
}

/// One row of keybindings.tsv: an event and what AppKit makes of it.
struct Row {
    characters: String,
    unmodified: String,
    flags: F,
    /// What interpretKeyEvents: sends, and what handleEvent: sends.
    interpreted: Vec<String>,
    handled: Vec<String>,
}

fn table() -> Vec<Row> {
    let text = |field: &str| -> String {
        if field == "-" {
            return String::new();
        }
        field.split('+').map(|h| char::from_u32(u32::from_str_radix(h, 16).expect("hex")).expect("a char")).collect()
    };
    let list = |field: &str| -> Vec<String> {
        if field == "-" {
            Vec::new()
        } else if field.starts_with("insert ") {
            vec![field.to_string()]
        } else {
            field.split(',').map(str::to_string).collect()
        }
    };
    include_str!("keybindings.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let interpreted = list(f[3]);
            let handled = if f[4] == "=" { interpreted.clone() } else { list(f[4]) };
            Row {
                characters: text(f[0]),
                unmodified: text(f[1]),
                flags: F(usize::from_str_radix(f[2], 16).expect("hex flags")),
                interpreted,
                handled,
            }
        })
        .collect()
}

fn describe(row: &Row) -> String {
    format!("{:?} ({:?}) with {:#x}", row.characters, row.unmodified, row.flags.0)
}

/// Every row through an input context, as text views take keys.
fn key_bindings_through_input_contexts(mtm: MainThreadMarker) -> Outcome {
    let client: Retained<TableClient> =
        unsafe { msg_send![super(TableClient::alloc(mtm).set_ivars(())), initWithFrame: frame()] };
    let context = client.inputContext().expect("an input context");
    take();
    let mut wrong = Vec::new();
    for row in table() {
        objc2::rc::autoreleasepool(|_| {
            let handled = context.handleEvent(&event(&row.characters, &row.unmodified, row.flags));
            let got = take();
            // Keys that do nothing aren't handled.
            if got != row.handled || handled == row.interpreted.is_empty() {
                wrong.push(format!("{}: {got:?} (handled {handled}), not {:?}", describe(&row), row.handled));
            }
        });
    }
    assert!(wrong.is_empty(), "{} of the table's keys differ:\n{}", wrong.len(), wrong.join("\n"));
    Ok(())
}

/// Every row through interpretKeyEvents:, which takes an NSArray.
fn key_bindings_through_responders(mtm: MainThreadMarker) -> Outcome {
    let Some(array_class) = AnyClass::get(c"NSArray") else {
        return Err("no NSArray yet");
    };
    let editor: Retained<Editor> =
        unsafe { msg_send![super(Editor::alloc(mtm).set_ivars(())), initWithFrame: frame()] };
    take();
    let mut wrong = Vec::new();
    for row in table() {
        objc2::rc::autoreleasepool(|_| {
            let event = event(&row.characters, &row.unmodified, row.flags);
            let array: Retained<AnyObject> = unsafe { msg_send![array_class, arrayWithObject: &*event] };
            let _: () = unsafe { msg_send![&*editor, interpretKeyEvents: &*array] };
            let got = take();
            if got != row.interpreted {
                wrong.push(format!("{}: {got:?}, not {:?}", describe(&row), row.interpreted));
            }
        });
    }
    assert!(wrong.is_empty(), "{} of the table's keys differ:\n{}", wrong.len(), wrong.join("\n"));
    Ok(())
}

fn text_input_clients(mtm: MainThreadMarker) -> Outcome {
    // Only views that take text from input methods have an input context.
    let plain = NSView::initWithFrame(NSView::alloc(mtm), frame());
    assert!(plain.inputContext().is_none());
    let client: Retained<TextClient> =
        unsafe { msg_send![super(TextClient::alloc(mtm).set_ivars(())), initWithFrame: frame()] };
    let context = client.inputContext().expect("an input context");
    assert!(client.inputContext().is_some_and(|again| std::ptr::eq(&*again, &*context)));
    let owner = context.client();
    assert!(std::ptr::eq(Retained::as_ptr(&owner).cast::<u8>(), Retained::as_ptr(&client).cast::<u8>()));

    // Keys handled by the context reach the client as text with no range
    // to replace, or as commands.
    take();
    assert!(context.handleEvent(&key("x", F(0))));
    assert_eq!(take(), ["insert x at none"]);
    assert!(context.handleEvent(&key("\r", F(0))));
    assert_eq!(take(), ["insertNewline:"]);
    assert!(context.handleEvent(&key("\u{1}", F::Control | F::Shift)));
    assert_eq!(take(), ["moveToBeginningOfParagraphAndModifySelection:"]);
    // Keys that do nothing aren't handled.
    assert!(!context.handleEvent(&key("a", F::Command | F::Control)));
    assert_eq!(take(), Vec::<String>::new());
    Ok(())
}

fn actions(mtm: MainThreadMarker) -> Outcome {
    let app = NSApplication::sharedApplication(mtm);
    let target: Retained<ActionTarget> = unsafe { msg_send![super(ActionTarget::alloc(mtm).set_ivars(())), init] };
    let sender = NSObject::new();
    take();
    // A target that has the action gets it, with the sender.
    assert!(unsafe { app.sendAction_to_from(sel!(copy:), Some(&target), Some(&sender)) });
    assert_eq!(take(), ["target copy: from a sender"]);
    // Nobody has this one.
    assert!(!unsafe { app.sendAction_to_from(sel!(conformanceNobodyHasThis:), None, None) });
    assert!(unsafe { app.targetForAction(sel!(conformanceNobodyHasThis:)) }.is_none());
    // Without a target, the application's delegate is asked too.
    app.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    let found = unsafe { app.targetForAction(sel!(conformanceDelegateAction:)) };
    assert!(
        found.is_some_and(|f| std::ptr::eq(Retained::as_ptr(&f).cast::<u8>(), Retained::as_ptr(&target).cast::<u8>()))
    );
    assert!(unsafe { app.sendAction_to_from(sel!(conformanceDelegateAction:), None, None) });
    assert_eq!(take(), ["delegate action"]);
    assert!(unsafe { app.tryToPerform_with(sel!(conformanceDelegateAction:), None) });
    assert_eq!(take(), ["delegate action"]);
    app.setDelegate(None);

    // Responders try up their chain.
    let parent: Retained<Parent> =
        unsafe { msg_send![super(Parent::alloc(mtm).set_ivars(())), initWithFrame: frame()] };
    let child = NSView::initWithFrame(NSView::alloc(mtm), frame());
    parent.addSubview(&child);
    assert!(unsafe { child.tryToPerform_with(sel!(moveLeft:), Some(&sender)) });
    assert_eq!(take(), ["parent moveLeft: from a sender"]);
    assert!(!unsafe { child.tryToPerform_with(sel!(conformanceNobodyHasThis:), None) });
    assert!(!app.isHidden());
    Ok(())
}

/// A test passes, or is skipped for the reason given.
type Outcome = Result<(), &'static str>;

type Test = (&'static str, fn(MainThreadMarker) -> Outcome);

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("commands_travel_up_the_chain", commands_travel_up_the_chain),
        ("key_bindings_through_input_contexts", key_bindings_through_input_contexts),
        ("key_bindings_through_responders", key_bindings_through_responders),
        ("text_input_clients", text_input_clients),
        ("actions", actions),
    ];
    for (name, test) in tests {
        match objc2::rc::autoreleasepool(|_| test(mtm)) {
            Ok(()) => println!("test {name} ... ok"),
            Err(reason) => println!("test {name} ... skipped ({reason})"),
        }
    }
}
