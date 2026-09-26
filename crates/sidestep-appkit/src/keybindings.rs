//! Key bindings: what `-[NSResponder interpretKeyEvents:]` makes of key
//! presses. Keys that type text reach the responder as `insertText:`;
//! editing keys become commands sent with `doCommandBySelector:` (the
//! arrows move, Backspace deletes backward, the Emacs-style Control keys
//! work, …), as AppKit's standard bindings have them; other Command and
//! Control presses become `noop:`, and some combinations do nothing.
//!
//! A binding names a key by the character it types without modifiers
//! (letters in lowercase), the modifiers held, and whether it's on the
//! keypad. [`BINDINGS`] lists every key AppKit binds, and every key whose
//! answer differs from the rule for unbound keys ([`unbound`]); both were
//! read off AppKit on a Mac, fed the events Sidestep makes for each key of
//! a US keyboard with every combination of Shift, Control, Option and
//! Command (conformance/tests/keybindings.tsv holds that table, and the
//! conformance test checks both platforms against it). A keypad key that
//! isn't listed takes its twin off the keypad's binding.

use std::ffi::CStr;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2_app_kit::{NSEvent, NSEventModifierFlags as Flags, NSResponder};
use objc2_foundation::{NSRange, NSString};

const SHIFT: u8 = 1;
const CONTROL: u8 = 2;
const OPTION: u8 = 4;
const COMMAND: u8 = 8;
/// On the numeric keypad (`NSEventModifierFlagNumericPad`), as the arrow
/// keys and Enter always are.
const KEYPAD: u8 = 16;

const ENTER: char = '\u{3}';
const BACKTAB: char = '\u{19}';
const ESCAPE: char = '\u{1b}';
const BACKSPACE: char = '\u{7f}';
const UP: char = '\u{F700}';
const DOWN: char = '\u{F701}';
const LEFT: char = '\u{F702}';
const RIGHT: char = '\u{F703}';
const F5: char = '\u{F708}';
const DELETE_FORWARD: char = '\u{F728}';
const HOME: char = '\u{F729}';
const END: char = '\u{F72B}';
const PAGE_UP: char = '\u{F72C}';
const PAGE_DOWN: char = '\u{F72D}';
const CLEAR: char = '\u{F739}';

/// What a listed key does.
#[derive(Debug, PartialEq)]
enum Bound {
    /// Send these commands, in order.
    Run(&'static [&'static CStr]),
    /// Type the key's characters, whatever they are.
    Insert,
    /// Nothing at all.
    Nothing,
    /// What unbound keys do, although the key off the keypad is bound.
    Unbound,
}

use Bound::{Insert, Nothing, Run, Unbound};

/// Key, modifiers, and what the key does.
type Binding = (char, u8, Bound);

const BINDINGS: &[Binding] = &[
    ('\r', 0, Run(&[c"insertNewline:"])),
    ('\r', SHIFT, Run(&[c"insertNewline:"])),
    ('\r', CONTROL, Run(&[c"insertLineBreak:"])),
    ('\r', SHIFT | CONTROL, Run(&[c"insertLineBreak:"])),
    ('\r', OPTION, Run(&[c"insertNewlineIgnoringFieldEditor:"])),
    ('\r', SHIFT | OPTION, Run(&[c"insertNewlineIgnoringFieldEditor:"])),
    (ENTER, 0, Run(&[c"insertNewline:"])),
    (ENTER, SHIFT, Insert),
    (ENTER, CONTROL, Run(&[c"insertLineBreak:"])),
    (ENTER, SHIFT | CONTROL, Insert),
    (ENTER, OPTION, Run(&[c"insertNewlineIgnoringFieldEditor:"])),
    (ENTER, SHIFT | COMMAND, Nothing),
    ('\n', 0, Run(&[c"insertNewline:"])),
    ('\n', SHIFT, Run(&[c"insertNewline:"])),
    ('\n', CONTROL, Run(&[c"insertLineBreak:"])),
    ('\n', SHIFT | CONTROL, Run(&[c"insertLineBreak:"])),
    ('\n', OPTION, Run(&[c"insertNewlineIgnoringFieldEditor:"])),
    ('\n', SHIFT | OPTION, Run(&[c"insertNewlineIgnoringFieldEditor:"])),
    ('\t', 0, Run(&[c"insertTab:"])),
    ('\t', CONTROL, Run(&[c"selectNextKeyView:"])),
    ('\t', OPTION, Run(&[c"insertTabIgnoringFieldEditor:"])),
    (BACKTAB, SHIFT, Run(&[c"insertBacktab:"])),
    (BACKTAB, SHIFT | CONTROL, Run(&[c"selectPreviousKeyView:"])),
    (ESCAPE, 0, Run(&[c"cancelOperation:"])),
    (ESCAPE, SHIFT, Run(&[c"cancelOperation:"])),
    (ESCAPE, OPTION, Run(&[c"complete:"])),
    (ESCAPE, SHIFT | OPTION, Run(&[c"complete:"])),
    (BACKSPACE, 0, Run(&[c"deleteBackward:"])),
    (BACKSPACE, SHIFT, Run(&[c"deleteBackward:"])),
    (BACKSPACE, CONTROL, Run(&[c"deleteBackwardByDecomposingPreviousCharacter:"])),
    (BACKSPACE, SHIFT | CONTROL, Run(&[c"deleteBackwardByDecomposingPreviousCharacter:"])),
    (BACKSPACE, OPTION, Run(&[c"deleteWordBackward:"])),
    (BACKSPACE, SHIFT | OPTION, Run(&[c"deleteWordBackward:"])),
    (BACKSPACE, CONTROL | OPTION, Run(&[c"deleteWordBackward:"])),
    (BACKSPACE, SHIFT | CONTROL | OPTION, Run(&[c"deleteWordBackward:"])),
    (BACKSPACE, COMMAND, Run(&[c"deleteToBeginningOfLine:"])),
    (BACKSPACE, SHIFT | COMMAND, Run(&[c"deleteToBeginningOfLine:"])),
    (DELETE_FORWARD, 0, Run(&[c"deleteForward:"])),
    (DELETE_FORWARD, SHIFT, Run(&[c"deleteForward:"])),
    (DELETE_FORWARD, OPTION, Run(&[c"deleteWordForward:"])),
    (DELETE_FORWARD, SHIFT | OPTION, Run(&[c"deleteWordForward:"])),
    (DELETE_FORWARD, SHIFT | KEYPAD, Unbound),
    (DELETE_FORWARD, SHIFT | OPTION | KEYPAD, Unbound),
    (CLEAR, 0, Run(&[c"delete:"])),
    (CLEAR, SHIFT, Run(&[c"delete:"])),
    (CLEAR, SHIFT | KEYPAD, Unbound),
    (LEFT, 0, Run(&[c"moveLeft:"])),
    (LEFT, SHIFT, Run(&[c"moveLeftAndModifySelection:"])),
    (LEFT, CONTROL, Run(&[c"moveToLeftEndOfLine:"])),
    (LEFT, SHIFT | CONTROL, Run(&[c"moveToLeftEndOfLineAndModifySelection:"])),
    (LEFT, OPTION, Run(&[c"moveWordLeft:"])),
    (LEFT, SHIFT | OPTION, Run(&[c"moveWordLeftAndModifySelection:"])),
    (LEFT, COMMAND, Run(&[c"moveToLeftEndOfLine:"])),
    (LEFT, SHIFT | COMMAND, Run(&[c"moveToLeftEndOfLineAndModifySelection:"])),
    (LEFT, CONTROL | COMMAND, Run(&[c"makeBaseWritingDirectionRightToLeft:"])),
    (LEFT, CONTROL | OPTION | COMMAND, Run(&[c"makeTextWritingDirectionRightToLeft:"])),
    (RIGHT, 0, Run(&[c"moveRight:"])),
    (RIGHT, SHIFT, Run(&[c"moveRightAndModifySelection:"])),
    (RIGHT, CONTROL, Run(&[c"moveToRightEndOfLine:"])),
    (RIGHT, SHIFT | CONTROL, Run(&[c"moveToRightEndOfLineAndModifySelection:"])),
    (RIGHT, OPTION, Run(&[c"moveWordRight:"])),
    (RIGHT, SHIFT | OPTION, Run(&[c"moveWordRightAndModifySelection:"])),
    (RIGHT, COMMAND, Run(&[c"moveToRightEndOfLine:"])),
    (RIGHT, SHIFT | COMMAND, Run(&[c"moveToRightEndOfLineAndModifySelection:"])),
    (RIGHT, CONTROL | COMMAND, Run(&[c"makeBaseWritingDirectionLeftToRight:"])),
    (RIGHT, CONTROL | OPTION | COMMAND, Run(&[c"makeTextWritingDirectionLeftToRight:"])),
    (UP, 0, Run(&[c"moveUp:"])),
    (UP, SHIFT, Run(&[c"moveUpAndModifySelection:"])),
    (UP, CONTROL, Run(&[c"scrollPageUp:"])),
    (UP, OPTION, Run(&[c"moveBackward:", c"moveToBeginningOfParagraph:"])),
    (UP, SHIFT | OPTION, Run(&[c"moveParagraphBackwardAndModifySelection:"])),
    (UP, COMMAND, Run(&[c"moveToBeginningOfDocument:"])),
    (UP, SHIFT | COMMAND, Run(&[c"moveToBeginningOfDocumentAndModifySelection:"])),
    (DOWN, 0, Run(&[c"moveDown:"])),
    (DOWN, SHIFT, Run(&[c"moveDownAndModifySelection:"])),
    (DOWN, CONTROL, Run(&[c"scrollPageDown:"])),
    (DOWN, OPTION, Run(&[c"moveForward:", c"moveToEndOfParagraph:"])),
    (DOWN, SHIFT | OPTION, Run(&[c"moveParagraphForwardAndModifySelection:"])),
    (DOWN, COMMAND, Run(&[c"moveToEndOfDocument:"])),
    (DOWN, SHIFT | COMMAND, Run(&[c"moveToEndOfDocumentAndModifySelection:"])),
    (DOWN, CONTROL | COMMAND, Run(&[c"makeBaseWritingDirectionNatural:"])),
    (DOWN, CONTROL | OPTION | COMMAND, Run(&[c"makeTextWritingDirectionNatural:"])),
    (HOME, 0, Run(&[c"scrollToBeginningOfDocument:"])),
    (HOME, SHIFT, Run(&[c"moveToBeginningOfDocumentAndModifySelection:"])),
    (END, 0, Run(&[c"scrollToEndOfDocument:"])),
    (END, SHIFT, Run(&[c"moveToEndOfDocumentAndModifySelection:"])),
    (PAGE_UP, 0, Run(&[c"scrollPageUp:"])),
    (PAGE_UP, SHIFT, Run(&[c"pageUpAndModifySelection:"])),
    (PAGE_UP, OPTION, Run(&[c"pageUp:"])),
    (PAGE_UP, SHIFT | OPTION, Run(&[c"pageUp:"])),
    (PAGE_UP, SHIFT | OPTION | KEYPAD, Unbound),
    (PAGE_DOWN, 0, Run(&[c"scrollPageDown:"])),
    (PAGE_DOWN, SHIFT, Run(&[c"pageDownAndModifySelection:"])),
    (PAGE_DOWN, OPTION, Run(&[c"pageDown:"])),
    (PAGE_DOWN, SHIFT | OPTION, Run(&[c"pageDown:"])),
    (PAGE_DOWN, SHIFT | OPTION | KEYPAD, Unbound),
    (F5, 0, Run(&[c"complete:"])),
    (F5, SHIFT, Run(&[c"complete:"])),
    ('"', SHIFT | CONTROL, Run(&[c"insertDoubleQuoteIgnoringSubstitution:"])),
    ('\'', CONTROL, Run(&[c"insertSingleQuoteIgnoringSubstitution:"])),
    ('*', SHIFT | CONTROL | KEYPAD, Insert),
    ('*', SHIFT | COMMAND | KEYPAD, Nothing),
    ('+', SHIFT | CONTROL | KEYPAD, Insert),
    ('+', SHIFT | COMMAND | KEYPAD, Nothing),
    ('-', SHIFT | CONTROL | KEYPAD, Insert),
    ('-', SHIFT | COMMAND | KEYPAD, Nothing),
    ('.', COMMAND, Run(&[c"cancelOperation:"])),
    ('.', SHIFT | CONTROL | KEYPAD, Insert),
    ('.', SHIFT | COMMAND | KEYPAD, Nothing),
    ('/', CONTROL, Run(&[c"insertRightToLeftSlash:"])),
    ('/', SHIFT | CONTROL | KEYPAD, Insert),
    ('/', SHIFT | COMMAND | KEYPAD, Nothing),
    ('=', SHIFT | CONTROL | KEYPAD, Insert),
    ('=', SHIFT | COMMAND | KEYPAD, Nothing),
    ('a', CONTROL, Run(&[c"moveToBeginningOfParagraph:"])),
    ('a', SHIFT | CONTROL, Run(&[c"moveToBeginningOfParagraphAndModifySelection:"])),
    ('b', CONTROL, Run(&[c"moveBackward:"])),
    ('b', SHIFT | CONTROL, Run(&[c"moveBackwardAndModifySelection:"])),
    ('b', CONTROL | OPTION, Run(&[c"moveWordBackward:"])),
    ('b', SHIFT | CONTROL | OPTION, Run(&[c"moveWordBackwardAndModifySelection:"])),
    ('d', CONTROL, Run(&[c"deleteForward:"])),
    ('e', CONTROL, Run(&[c"moveToEndOfParagraph:"])),
    ('e', SHIFT | CONTROL, Run(&[c"moveToEndOfParagraphAndModifySelection:"])),
    ('f', CONTROL, Run(&[c"moveForward:"])),
    ('f', SHIFT | CONTROL, Run(&[c"moveForwardAndModifySelection:"])),
    ('f', CONTROL | OPTION, Run(&[c"moveWordForward:"])),
    ('f', SHIFT | CONTROL | OPTION, Run(&[c"moveWordForwardAndModifySelection:"])),
    ('h', CONTROL, Run(&[c"deleteBackward:"])),
    ('k', CONTROL, Run(&[c"deleteToEndOfParagraph:"])),
    ('l', CONTROL, Run(&[c"centerSelectionInVisibleArea:"])),
    ('n', CONTROL, Run(&[c"moveDown:"])),
    ('n', SHIFT | CONTROL, Run(&[c"moveDownAndModifySelection:"])),
    ('o', CONTROL, Run(&[c"insertNewlineIgnoringFieldEditor:", c"moveBackward:"])),
    ('p', CONTROL, Run(&[c"moveUp:"])),
    ('p', SHIFT | CONTROL, Run(&[c"moveUpAndModifySelection:"])),
    ('t', CONTROL, Run(&[c"transpose:"])),
    ('v', CONTROL, Run(&[c"pageDown:"])),
    ('v', SHIFT | CONTROL, Run(&[c"pageDownAndModifySelection:"])),
    ('y', CONTROL, Run(&[c"yank:"])),
];

fn modifiers(flags: Flags) -> u8 {
    [
        (Flags::Shift, SHIFT),
        (Flags::Control, CONTROL),
        (Flags::Option, OPTION),
        (Flags::Command, COMMAND),
        (Flags::NumericPad, KEYPAD),
    ]
    .into_iter()
    .filter(|(flag, _)| flags.contains(*flag))
    .fold(0, |acc, (_, bit)| acc | bit)
}

/// What `key` held with `mods` does, if the table says: its own entry, or
/// for a keypad key its twin's off the keypad.
fn binding(key: char, mods: u8) -> Option<&'static Bound> {
    let find = |mods: u8| BINDINGS.iter().find(|(k, m, _)| *k == key && *m == mods).map(|(_, _, b)| b);
    let found = find(mods).or_else(|| if mods & KEYPAD != 0 { find(mods & !KEYPAD) } else { None });
    found.filter(|b| **b != Unbound)
}

/// AppKit's private-use characters for keys that don't type (arrows,
/// function keys, …).
fn is_function_key(c: char) -> bool {
    ('\u{F700}'..='\u{F8FF}').contains(&c)
}

/// Whether a key's characters are text to insert, rather than controls or
/// function-key characters.
fn is_text(characters: &str) -> bool {
    !characters.is_empty() && characters.chars().all(|c| !c.is_control() && !is_function_key(c))
}

/// Interpret each event of an `NSArray` of key events for `responder`.
pub(crate) fn interpret_all(responder: &NSResponder, events: &AnyObject) {
    // SAFETY: interpretKeyEvents: takes an array of events; count and
    // objectAtIndex: are NSArray's.
    let count: usize = unsafe { msg_send![events, count] };
    for i in 0..count {
        // SAFETY: as above; the array holds NSEvents.
        let event: Retained<NSEvent> = unsafe { msg_send![events, objectAtIndex: i] };
        interpret_for(responder, &event, Via::Responder);
    }
}

/// What a key press does.
#[derive(Debug, PartialEq)]
enum Action<'a> {
    Insert(&'a str),
    Commands(&'static [&'static CStr]),
    /// Nothing: a key AppKit ignores, or a dead key, whose sequence the
    /// next key finishes.
    Nothing,
}

const NOOP: &[&CStr] = &[c"noop:"];

/// What a key press does: its binding, or else the rule for unbound keys.
/// `composing`: the key started or continued a compose sequence, and
/// types nothing yet.
fn action<'a>(characters: &'a str, unmodified: &str, flags: Flags, composing: bool) -> Action<'a> {
    if composing && characters.is_empty() {
        return Action::Nothing;
    }
    let mods = modifiers(flags);
    let key = unmodified.chars().next().map(|c| c.to_ascii_lowercase());
    match key.and_then(|k| binding(k, mods)) {
        Some(Run(commands)) => Action::Commands(commands),
        Some(Insert) => Action::Insert(characters),
        Some(Nothing) => Action::Nothing,
        Some(Unbound) | None => unbound(characters, unmodified, mods),
    }
}

/// What a key no binding names does:
///
/// - with Command: `noop:`, but nothing at all with Control or Option too,
///   unless it's a function key;
/// - a key that types nothing inserts that (`noop:` for function keys);
/// - with Control but not Option: `noop:`;
/// - text, or anything but a function key with Option: inserted;
/// - the rest (controls, function keys): `noop:`.
fn unbound<'a>(characters: &'a str, unmodified: &str, mods: u8) -> Action<'a> {
    let function = if characters.is_empty() { unmodified } else { characters }.chars().any(is_function_key);
    if mods & COMMAND != 0 {
        return if mods & (CONTROL | OPTION) != 0 && !function { Action::Nothing } else { Action::Commands(NOOP) };
    }
    if characters.is_empty() {
        return if function { Action::Commands(NOOP) } else { Action::Insert(characters) };
    }
    if mods & CONTROL != 0 && mods & OPTION == 0 {
        return Action::Commands(NOOP);
    }
    if is_text(characters) || (mods & OPTION != 0 && !function) {
        return Action::Insert(characters);
    }
    Action::Commands(NOOP)
}

/// Where a key press is being interpreted.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Via {
    /// `-[NSResponder interpretKeyEvents:]`.
    Responder,
    /// `-[NSTextInputContext handleEvent:]`, which leaves out a few
    /// commands `interpretKeyEvents:` sends (seen on macOS: the writing
    /// direction keys and Control-/).
    InputContext,
}

/// Commands `handleEvent:` doesn't send.
fn left_out_by_input_context(commands: &[&CStr]) -> bool {
    commands.iter().any(|c| {
        let name = c.to_bytes();
        name.starts_with(b"makeBaseWritingDirection")
            || name.starts_with(b"makeTextWritingDirection")
            || name == b"insertRightToLeftSlash:"
    })
}

/// Send what a key press does to `target`: a responder, or an input
/// context's client. Clients of input methods take text with
/// `insertText:replacementRange:`, others with `insertText:`. False when
/// the key does nothing, as `handleEvent:` answers.
pub(crate) fn interpret_for(target: &AnyObject, event: &NSEvent, via: Via) -> bool {
    let characters = event.characters().map(|s| s.to_string()).unwrap_or_default();
    let unmodified = event.charactersIgnoringModifiers().map(|s| s.to_string()).unwrap_or_default();
    let client = crate::inputcontext::is_client(target);
    let composing = crate::event::composing(event);
    let action = action(&characters, &unmodified, event.modifierFlags(), composing.is_some_and(|p| !p.is_empty()));
    let inserted = matches!(action, Action::Insert(_));
    let mut handled = true;
    match action {
        Action::Insert(text) => {
            let text = NSString::from_str(text);
            if client {
                let none = NSRange::new(crate::inputcontext::NOT_FOUND, 0);
                // SAFETY: insertText:replacementRange: takes the text and
                // the range it replaces.
                let _: () = unsafe { msg_send![target, insertText: &*text, replacementRange: none] };
            } else {
                // SAFETY: insertText: takes the text to insert.
                let _: () = unsafe { msg_send![target, insertText: &*text] };
            }
        }
        Action::Commands(commands) if via == Via::InputContext && left_out_by_input_context(commands) => {}
        Action::Commands(commands) => {
            for name in commands {
                command(target, Sel::register(name));
            }
        }
        Action::Nothing => handled = false,
    }
    // A dead key's accent shows as marked text until the sequence ends;
    // inserted text has already replaced it.
    if client && let Some(pending) = composing {
        handled = true;
        if !pending.is_empty() {
            crate::inputcontext::mark(target, pending);
        } else if !inserted {
            crate::inputcontext::mark(target, "");
        }
    }
    handled
}

fn command(target: &AnyObject, selector: Sel) {
    // SAFETY: doCommandBySelector: takes a selector.
    let _: () = unsafe { msg_send![target, doCommandBySelector: selector] };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(key: char, mods: u8) -> Vec<&'static str> {
        match binding(key, mods) {
            Some(Run(commands)) => commands.iter().map(|c| c.to_str().unwrap()).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn shift_is_a_binding_of_its_own() {
        // AppKit has no Shift fallback: Control-Shift-A selects, and
        // Control-Shift-K, unlike Control-K, doesn't delete.
        assert_eq!(names('a', CONTROL | SHIFT), ["moveToBeginningOfParagraphAndModifySelection:"]);
        assert!(binding('k', CONTROL | SHIFT).is_none());
        assert_eq!(names(LEFT, SHIFT | KEYPAD), ["moveLeftAndModifySelection:"]);
        assert!(binding('a', 0).is_none());
        assert!(binding('a', SHIFT).is_none());
        assert!(binding('a', COMMAND).is_none());
    }

    #[test]
    fn keypad_keys_take_their_twins_bindings() {
        assert_eq!(names(DELETE_FORWARD, KEYPAD), ["deleteForward:"]);
        // Unless the table says otherwise.
        assert!(binding(DELETE_FORWARD, SHIFT | KEYPAD).is_none());
        assert_eq!(names(DELETE_FORWARD, SHIFT), ["deleteForward:"]);
    }

    #[test]
    fn text_is_what_types() {
        assert!(is_text("a") && is_text("é") && is_text("✓"));
        assert!(!is_text("") && !is_text("\u{1}") && !is_text("\u{7f}") && !is_text("\u{F704}"));
    }

    /// Cases from conformance/tests/keybindings.tsv, which the conformance
    /// test checks against AppKit, with the events a US keyboard makes.
    #[test]
    fn keys_do_what_appkit_does() {
        let arrow = Flags::Function | Flags::NumericPad;
        let cases: &[(&str, &str, Flags, &[&str])] = &[
            ("a", "a", Flags(0), &["insert a"]),
            ("A", "A", Flags::Shift, &["insert A"]),
            ("é", "é", Flags(0), &["insert é"]),
            ("a", "a", Flags::Option, &["insert a"]),
            ("a", "a", Flags::Command, &["noop:"]),
            ("a", "a", Flags::Command | Flags::Control, &[]),
            ("1", "1", Flags::Control, &["noop:"]),
            ("\u{11}", "q", Flags::Control | Flags::Option, &["insert \u{11}"]),
            ("\u{F704}", "\u{F704}", Flags::Function, &["noop:"]),
            ("\u{F704}", "\u{F704}", Flags::Function | Flags::Command | Flags::Option, &["noop:"]),
            ("\r", "\r", Flags(0), &["insertNewline:"]),
            ("\r", "\r", Flags::Option, &["insertNewlineIgnoringFieldEditor:"]),
            ("\r", "\r", Flags::Control, &["insertLineBreak:"]),
            ("\u{3}", "\u{3}", Flags::NumericPad | Flags::Shift, &["insert \u{3}"]),
            ("\t", "\t", Flags::Control, &["selectNextKeyView:"]),
            ("\u{19}", "\u{19}", Flags::Shift, &["insertBacktab:"]),
            ("\u{1b}", "\u{1b}", Flags::Option, &["complete:"]),
            ("\u{7f}", "\u{7f}", Flags::Option, &["deleteWordBackward:"]),
            ("\u{F700}", "\u{F700}", arrow | Flags::Option, &["moveBackward:", "moveToBeginningOfParagraph:"]),
            (
                "\u{F700}",
                "\u{F700}",
                arrow | Flags::Option | Flags::Shift,
                &["moveParagraphBackwardAndModifySelection:"],
            ),
            ("\u{F701}", "\u{F701}", arrow | Flags::Control, &["scrollPageDown:"]),
            (
                "\u{F703}",
                "\u{F703}",
                arrow | Flags::Control | Flags::Shift,
                &["moveToRightEndOfLineAndModifySelection:"],
            ),
            ("\u{F72C}", "\u{F72C}", Flags::Function | Flags::Shift, &["pageUpAndModifySelection:"]),
            ("\u{F708}", "\u{F708}", Flags::Function, &["complete:"]),
            ("\u{1}", "a", Flags::Control, &["moveToBeginningOfParagraph:"]),
            ("\u{1}", "A", Flags::Control | Flags::Shift, &["moveToBeginningOfParagraphAndModifySelection:"]),
            ("\u{b}", "K", Flags::Control | Flags::Shift, &["noop:"]),
            ("\u{2}", "b", Flags::Control | Flags::Option, &["moveWordBackward:"]),
            ("\u{f}", "o", Flags::Control, &["insertNewlineIgnoringFieldEditor:", "moveBackward:"]),
            ("*", "*", Flags::NumericPad | Flags::Shift | Flags::Command, &[]),
            ("", "*", Flags::Control | Flags::Option, &["insert "]),
        ];
        for (characters, unmodified, flags, expected) in cases {
            let got: Vec<String> = match action(characters, unmodified, *flags, false) {
                Action::Insert(text) => vec![format!("insert {text}")],
                Action::Commands(c) => c.iter().map(|c| c.to_str().unwrap().to_string()).collect(),
                Action::Nothing => vec![],
            };
            assert_eq!(got, *expected, "{characters:?} {unmodified:?} {:#x}", flags.0);
        }
        // A dead key types nothing yet.
        assert_eq!(action("", "e", Flags(0), true), Action::Nothing);
    }

    #[test]
    fn bindings_are_unique() {
        for (i, (k, m, _)) in BINDINGS.iter().enumerate() {
            assert!(BINDINGS[i + 1..].iter().all(|(k2, m2, _)| (k, m) != (k2, m2)), "{k:?} {m}");
        }
    }
}
