//! Key bindings: what `-[NSResponder interpretKeyEvents:]` makes of key
//! presses. Keys that type text reach the responder as `insertText:`;
//! editing keys become commands sent with `doCommandBySelector:` (the
//! arrows move, Backspace deletes backward, the Emacs-style Control keys
//! work, …), as AppKit's standard bindings have them; Command-key presses
//! and keys that neither type nor edit become `noop:`.
//!
//! A binding names a key by the character it types without modifiers
//! (letters in lowercase) and the modifiers held. A key held with Shift
//! and bound only without it takes the binding without it.

use std::ffi::CStr;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2_app_kit::{NSEvent, NSEventModifierFlags as Flags, NSResponder};
use objc2_foundation::NSString;

const SHIFT: u8 = 1;
const CONTROL: u8 = 2;
const OPTION: u8 = 4;
const COMMAND: u8 = 8;

const LEFT: char = '\u{F702}';
const RIGHT: char = '\u{F703}';
const UP: char = '\u{F700}';
const DOWN: char = '\u{F701}';
const DELETE_FORWARD: char = '\u{F728}';
const HOME: char = '\u{F729}';
const END: char = '\u{F72B}';
const PAGE_UP: char = '\u{F72C}';
const PAGE_DOWN: char = '\u{F72D}';

/// Key, modifiers, and the commands it sends, in order.
type Binding = (char, u8, &'static [&'static CStr]);

const BINDINGS: &[Binding] = &[
    ('\r', 0, &[c"insertNewline:"]),
    ('\u{3}', 0, &[c"insertNewline:"]),
    ('\t', 0, &[c"insertTab:"]),
    ('\u{19}', 0, &[c"insertBacktab:"]),
    ('\u{1b}', 0, &[c"cancelOperation:"]),
    ('\u{7f}', 0, &[c"deleteBackward:"]),
    ('\u{7f}', OPTION, &[c"deleteWordBackward:"]),
    ('\u{7f}', COMMAND, &[c"deleteToBeginningOfLine:"]),
    ('\u{7f}', CONTROL, &[c"deleteBackwardByDecomposingPreviousCharacter:"]),
    (DELETE_FORWARD, 0, &[c"deleteForward:"]),
    (DELETE_FORWARD, OPTION, &[c"deleteWordForward:"]),
    (LEFT, 0, &[c"moveLeft:"]),
    (RIGHT, 0, &[c"moveRight:"]),
    (UP, 0, &[c"moveUp:"]),
    (DOWN, 0, &[c"moveDown:"]),
    (LEFT, SHIFT, &[c"moveLeftAndModifySelection:"]),
    (RIGHT, SHIFT, &[c"moveRightAndModifySelection:"]),
    (UP, SHIFT, &[c"moveUpAndModifySelection:"]),
    (DOWN, SHIFT, &[c"moveDownAndModifySelection:"]),
    (LEFT, OPTION, &[c"moveWordLeft:"]),
    (RIGHT, OPTION, &[c"moveWordRight:"]),
    (UP, OPTION, &[c"moveBackward:", c"moveToBeginningOfParagraph:"]),
    (DOWN, OPTION, &[c"moveForward:", c"moveToEndOfParagraph:"]),
    (LEFT, OPTION | SHIFT, &[c"moveWordLeftAndModifySelection:"]),
    (RIGHT, OPTION | SHIFT, &[c"moveWordRightAndModifySelection:"]),
    (LEFT, COMMAND, &[c"moveToLeftEndOfLine:"]),
    (RIGHT, COMMAND, &[c"moveToRightEndOfLine:"]),
    (UP, COMMAND, &[c"moveToBeginningOfDocument:"]),
    (DOWN, COMMAND, &[c"moveToEndOfDocument:"]),
    (LEFT, COMMAND | SHIFT, &[c"moveToLeftEndOfLineAndModifySelection:"]),
    (RIGHT, COMMAND | SHIFT, &[c"moveToRightEndOfLineAndModifySelection:"]),
    (UP, COMMAND | SHIFT, &[c"moveToBeginningOfDocumentAndModifySelection:"]),
    (DOWN, COMMAND | SHIFT, &[c"moveToEndOfDocumentAndModifySelection:"]),
    (LEFT, CONTROL, &[c"moveToLeftEndOfLine:"]),
    (RIGHT, CONTROL, &[c"moveToRightEndOfLine:"]),
    (HOME, 0, &[c"scrollToBeginningOfDocument:"]),
    (END, 0, &[c"scrollToEndOfDocument:"]),
    (PAGE_UP, 0, &[c"scrollPageUp:"]),
    (PAGE_DOWN, 0, &[c"scrollPageDown:"]),
    (HOME, SHIFT, &[c"moveToBeginningOfDocumentAndModifySelection:"]),
    (END, SHIFT, &[c"moveToEndOfDocumentAndModifySelection:"]),
    (PAGE_UP, OPTION, &[c"pageUp:"]),
    (PAGE_DOWN, OPTION, &[c"pageDown:"]),
    ('a', CONTROL, &[c"moveToBeginningOfParagraph:"]),
    ('b', CONTROL, &[c"moveBackward:"]),
    ('d', CONTROL, &[c"deleteForward:"]),
    ('e', CONTROL, &[c"moveToEndOfParagraph:"]),
    ('f', CONTROL, &[c"moveForward:"]),
    ('h', CONTROL, &[c"deleteBackward:"]),
    ('k', CONTROL, &[c"deleteToEndOfParagraph:"]),
    ('l', CONTROL, &[c"centerSelectionInVisibleArea:"]),
    ('n', CONTROL, &[c"moveDown:"]),
    ('o', CONTROL, &[c"insertNewlineIgnoringFieldEditor:", c"moveBackward:"]),
    ('p', CONTROL, &[c"moveUp:"]),
    ('t', CONTROL, &[c"transpose:"]),
    ('v', CONTROL, &[c"pageDown:"]),
    ('y', CONTROL, &[c"yank:"]),
];

fn modifiers(flags: Flags) -> u8 {
    [(Flags::Shift, SHIFT), (Flags::Control, CONTROL), (Flags::Option, OPTION), (Flags::Command, COMMAND)]
        .into_iter()
        .filter(|(flag, _)| flags.contains(*flag))
        .fold(0, |acc, (_, bit)| acc | bit)
}

/// The commands `key` held with `mods` sends, if it's bound.
fn binding(key: char, mods: u8) -> Option<&'static [&'static CStr]> {
    let find = |mods: u8| BINDINGS.iter().find(|(k, m, _)| *k == key && *m == mods).map(|(_, _, c)| *c);
    find(mods).or_else(|| if mods & SHIFT != 0 { find(mods & !SHIFT) } else { None })
}

/// Whether a key's characters are text to insert, rather than controls or
/// AppKit's function-key characters.
fn is_text(characters: &str) -> bool {
    !characters.is_empty() && characters.chars().all(|c| !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c))
}

/// Interpret each event of an `NSArray` of key events for `responder`.
pub(crate) fn interpret_all(responder: &NSResponder, events: &AnyObject) {
    // SAFETY: interpretKeyEvents: takes an array of events; count and
    // objectAtIndex: are NSArray's.
    let count: usize = unsafe { msg_send![events, count] };
    for i in 0..count {
        // SAFETY: as above; the array holds NSEvents.
        let event: Retained<NSEvent> = unsafe { msg_send![events, objectAtIndex: i] };
        interpret(responder, &event);
    }
}

/// What a key press does.
#[derive(Debug, PartialEq)]
enum Action<'a> {
    Insert(&'a str),
    Commands(&'static [&'static CStr]),
    /// A dead key: the next key finishes what it types.
    Nothing,
}

const NOOP: &[&CStr] = &[c"noop:"];

fn action<'a>(characters: &'a str, unmodified: &str, flags: Flags) -> Action<'a> {
    let mods = modifiers(flags);
    let key = unmodified.chars().next().map(|c| c.to_ascii_lowercase());
    if let Some(commands) = key.and_then(|k| binding(k, mods)) {
        return Action::Commands(commands);
    }
    if characters.is_empty() {
        return Action::Nothing;
    }
    if mods & COMMAND != 0 || !is_text(characters) {
        return Action::Commands(NOOP);
    }
    Action::Insert(characters)
}

fn interpret(responder: &NSResponder, event: &NSEvent) {
    let characters = event.characters().map(|s| s.to_string()).unwrap_or_default();
    let unmodified = event.charactersIgnoringModifiers().map(|s| s.to_string()).unwrap_or_default();
    match action(&characters, &unmodified, event.modifierFlags()) {
        Action::Insert(text) => {
            let text = NSString::from_str(text);
            // SAFETY: insertText: takes the text to insert.
            let _: () = unsafe { msg_send![responder, insertText: &*text] };
        }
        Action::Commands(commands) => {
            for name in commands {
                command(responder, Sel::register(name));
            }
        }
        Action::Nothing => {}
    }
}

fn command(responder: &NSResponder, selector: Sel) {
    // SAFETY: doCommandBySelector: takes a selector.
    let _: () = unsafe { msg_send![responder, doCommandBySelector: selector] };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(key: char, mods: u8) -> Vec<&'static str> {
        binding(key, mods).unwrap_or(&[]).iter().map(|c| c.to_str().unwrap()).collect()
    }

    #[test]
    fn shift_falls_back_to_the_unshifted_binding() {
        assert_eq!(names('a', CONTROL | SHIFT), ["moveToBeginningOfParagraph:"]);
        assert_eq!(names('\u{19}', SHIFT), ["insertBacktab:"]);
        assert_eq!(names(LEFT, SHIFT), ["moveLeftAndModifySelection:"]);
        assert!(binding('a', 0).is_none());
        assert!(binding('a', SHIFT).is_none());
        assert!(binding('a', COMMAND).is_none());
    }

    #[test]
    fn text_is_what_types() {
        assert!(is_text("a") && is_text("é") && is_text("✓"));
        assert!(!is_text("") && !is_text("\u{1}") && !is_text("\u{7f}") && !is_text("\u{F704}"));
    }

    /// The same keys as conformance/tests/appkit_keybindings.rs, which
    /// checks them against AppKit.
    #[test]
    fn keys_do_what_appkit_does() {
        let arrow = Flags::Function | Flags::NumericPad;
        let cases: &[(&str, &str, Flags, &[&str])] = &[
            ("a", "a", Flags(0), &["insert a"]),
            ("A", "A", Flags::Shift, &["insert A"]),
            ("é", "é", Flags(0), &["insert é"]),
            ("a", "a", Flags::Control | Flags::Option, &["insert a"]),
            ("a", "a", Flags::Command, &["noop:"]),
            ("\u{F704}", "\u{F704}", Flags::Function, &["noop:"]),
            ("\r", "\r", Flags(0), &["insertNewline:"]),
            ("\u{19}", "\u{19}", Flags::Shift, &["insertBacktab:"]),
            ("\u{7f}", "\u{7f}", Flags::Option, &["deleteWordBackward:"]),
            ("\u{F700}", "\u{F700}", arrow | Flags::Option, &["moveBackward:", "moveToBeginningOfParagraph:"]),
            (
                "\u{F701}",
                "\u{F701}",
                arrow | Flags::Command | Flags::Shift,
                &["moveToEndOfDocumentAndModifySelection:"],
            ),
            ("\u{1}", "a", Flags::Control, &["moveToBeginningOfParagraph:"]),
            ("\u{1}", "A", Flags::Control | Flags::Shift, &["moveToBeginningOfParagraph:"]),
            ("\u{f}", "o", Flags::Control, &["insertNewlineIgnoringFieldEditor:", "moveBackward:"]),
            ("", "e", Flags::Option, &[]),
        ];
        for (characters, unmodified, flags, expected) in cases {
            let got: Vec<String> = match action(characters, unmodified, *flags) {
                Action::Insert(text) => vec![format!("insert {text}")],
                Action::Commands(c) => c.iter().map(|c| c.to_str().unwrap().to_string()).collect(),
                Action::Nothing => vec![],
            };
            assert_eq!(got, *expected, "{characters:?} {:#x}", flags.0);
        }
    }

    #[test]
    fn bindings_are_unique() {
        for (i, (k, m, _)) in BINDINGS.iter().enumerate() {
            assert!(BINDINGS[i + 1..].iter().all(|(k2, m2, _)| (k, m) != (k2, m2)), "{k:?} {m}");
        }
    }
}
