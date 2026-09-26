//! Key equivalents: whether a key event is a menu item's, and how an item's
//! shortcut reads.
//!
//! An item's key equivalent and modifier mask are turned into a
//! [`Shortcut`] once, when either is set, so matching a key event against a
//! menu allocates nothing and sends no message per item. The event is read
//! once per menu ([`Pressed::of`]).
//!
//! A key event is an item's when:
//!
//! 1. its `charactersIgnoringModifiers` are the key equivalent, the event's
//!    Command, Option and Control are exactly the mask's, and Shift agrees:
//!    it is in both, in neither, or only in the event while the key
//!    equivalent is itself a shifted character (it differs from its
//!    lowercase, as `W` does, or from what the key types without Shift, as
//!    `!` does on the key that types `1`);
//! 2. or the mask and the event both have Shift and the key equivalent is
//!    what the key types without Shift: `z` with Command-Shift matches the
//!    `Z` that Command-Shift-Z types, `1` the `!`.
//!
//! An empty key equivalent never matches. Caps Lock, Function and Numeric
//! Pad don't count, so function keys (`NSF1FunctionKey`…, which set
//! Function) and Return and Escape match masks without them.
//!
//! What a key types without Shift comes from the key's letter when it is
//! one (so it follows the keyboard layout), else from the key's position
//! on a US keyboard (the key code table in `keycode_table.rs`): the
//! platform doesn't hand key events the unshifted level of their keymap.
//! Synthesized events on macOS can't tell some of these cases apart
//! (`conformance/tests/menus.rs` checks only the ones a real keyboard and
//! a synthesized event agree on); the rule gives what a real keyboard
//! does.
//!
//! Labels name modifiers as the platform maps them: Control is Ctrl,
//! Option is Alt and Command the Super key.

use objc2::msg_send;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_foundation::NSString;

use crate::keycodes::{KEY_CODES, Seen};

const COMMAND: usize = NSEventModifierFlags::Command.0;
const OPTION: usize = NSEventModifierFlags::Option.0;
const CONTROL: usize = NSEventModifierFlags::Control.0;
const SHIFT: usize = NSEventModifierFlags::Shift.0;
/// The modifiers that must equal the mask's.
const EXACT: usize = COMMAND | OPTION | CONTROL;

/// A key equivalent's key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Key {
    /// No key equivalent: never matches.
    #[default]
    None,
    One(char),
    /// Several characters, compared as strings (no keyboard types these).
    Many,
}

/// An item's key equivalent and mask, ready to match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shortcut {
    pub key: Key,
    /// Command, Option, Control and Shift of the mask.
    pub mask: usize,
    /// The key differs from its lowercase (an uppercase letter), so typing
    /// it takes Shift.
    pub upper: bool,
}

impl Shortcut {
    pub(crate) fn new(key: &NSString, mask: NSEventModifierFlags) -> Shortcut {
        let (first, count) = first_char(key);
        let key = match (first, count) {
            (None, _) => Key::None,
            (Some(c), 1) => Key::One(c),
            _ => Key::Many,
        };
        let upper = matches!(key, Key::One(c) if lowercase(c) != c);
        Shortcut { key, mask: mask.0 & (EXACT | SHIFT), upper }
    }
}

/// What a key-down event pressed, read once for a menu's items.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pressed {
    /// `charactersIgnoringModifiers`, when it is one character.
    key: Option<char>,
    /// It is several characters.
    many: bool,
    flags: usize,
    /// What the key types without Shift, when Shift is held.
    unshifted: Option<char>,
}

impl Pressed {
    /// A key-down event's key; None for other events.
    pub(crate) fn of(event: &NSEvent) -> Option<Pressed> {
        if event.r#type() != NSEventType::KeyDown {
            return None;
        }
        let ignoring = event.charactersIgnoringModifiers();
        let (key, count) = ignoring.as_deref().map_or((None, 0), first_char);
        let flags = event.modifierFlags().0;
        let unshifted = if flags & SHIFT != 0 { key.and_then(|k| unshifted(k, event.keyCode())) } else { key };
        Some(Pressed { key: if count == 1 { key } else { None }, many: count > 1, flags, unshifted })
    }

    /// Made directly, for tests.
    #[cfg(test)]
    fn new(key: char, flags: usize, unshifted: char) -> Pressed {
        Pressed { key: Some(key), many: false, flags, unshifted: Some(unshifted) }
    }
}

/// Whether the key is F10 without Command, Option or Control, which opens
/// the menu bar.
pub(crate) fn is_f10(pressed: &Pressed) -> bool {
    pressed.key == Some('\u{F70D}') && pressed.flags & EXACT == 0
}

/// Whether `pressed` is `shortcut`'s key. `many` compares a key equivalent
/// of several characters with the event's, which only such key
/// equivalents need.
pub(crate) fn matches(shortcut: &Shortcut, pressed: &Pressed, many: impl FnOnce() -> bool) -> bool {
    if shortcut.mask & EXACT != pressed.flags & EXACT {
        return false;
    }
    let (mask_shift, event_shift) = (shortcut.mask & SHIFT != 0, pressed.flags & SHIFT != 0);
    match shortcut.key {
        Key::None => false,
        Key::Many => pressed.many && mask_shift == event_shift && many(),
        Key::One(key) => {
            if pressed.key == Some(key) {
                let shifted = shortcut.upper || pressed.unshifted.is_some_and(|u| u != key);
                if mask_shift == event_shift || (event_shift && shifted) {
                    return true;
                }
            }
            mask_shift && event_shift && pressed.unshifted == Some(key)
        }
    }
}

/// The first character of `s` and how many it has (0, 1 or 2 for more).
pub(crate) fn first_char(s: &NSString) -> (Option<char>, usize) {
    let len = s.length();
    if len == 0 {
        return (None, 0);
    }
    // SAFETY: characterAtIndex: takes an index below the length.
    let unit = |i: usize| -> u16 { unsafe { msg_send![s, characterAtIndex: i] } };
    let first = unit(0);
    let (c, units) = if (0xD800..0xDC00).contains(&first) && len > 1 {
        let second = unit(1);
        (char::decode_utf16([first, second]).next().and_then(Result::ok), 2)
    } else {
        (char::from_u32(first.into()), 1)
    };
    (c, if len > units { 2 } else { 1 })
}

fn lowercase(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// What the key with macOS key code `code` types on a US keyboard, by key
/// code: the key code table's characters, for the codes below 128.
const US: [u8; 128] = {
    let mut table = [0u8; 128];
    let mut i = 0;
    while i < KEY_CODES.len() {
        let code = KEY_CODES[i].1 as usize;
        if let Seen::Char(s) = &KEY_CODES[i].2 {
            let bytes = s.as_bytes();
            if code < 128 && bytes.len() == 1 && bytes[0] >= 0x20 && bytes[0] < 0x7f {
                table[code] = bytes[0];
            }
        }
        i += 1;
    }
    table
};

/// What a key typing `key` with Shift types without it: a letter's
/// lowercase, else the US keyboard's character at the key's position.
fn unshifted(key: char, code: u16) -> Option<char> {
    let lower = lowercase(key);
    if lower != key {
        return Some(lower);
    }
    match US.get(code as usize) {
        Some(&b) if b != 0 => Some(b as char),
        _ => Some(key),
    }
}

/// How a shortcut reads in a menu: its modifiers, then its key, joined by
/// `+` ("Ctrl+Shift+Z", "Alt+F4", "Esc"). Empty without a key.
pub(crate) fn label(key: &str, mask: NSEventModifierFlags) -> String {
    let mut chars = key.chars();
    let Some(first) = chars.next() else { return String::new() };
    let one = chars.next().is_none();
    let upper = one && lowercase(first) != first;
    let mut out = String::new();
    let mask = mask.0;
    for (bit, name) in [(CONTROL, "Ctrl"), (OPTION, "Alt"), (SHIFT, "Shift"), (COMMAND, "Super")] {
        if mask & bit != 0 || (bit == SHIFT && upper) {
            out.push_str(name);
            out.push('+');
        }
    }
    if !one {
        out.push_str(&key.to_uppercase());
        return out;
    }
    match key_name(first) {
        Some(name) => out.push_str(name),
        None if ('\u{F704}'..='\u{F726}').contains(&first) => {
            out.push('F');
            out.push_str(&(first as u32 - 0xF704 + 1).to_string());
        }
        None => out.extend(first.to_uppercase()),
    }
    out
}

/// The names of keys that type no character of their own.
fn key_name(c: char) -> Option<&'static str> {
    Some(match c {
        '\r' | '\u{3}' => "Enter",
        '\u{1b}' => "Esc",
        '\t' | '\u{19}' => "Tab",
        ' ' => "Space",
        '\u{7f}' | '\u{8}' => "Backspace",
        '\u{F728}' => "Delete",
        '\u{F700}' => "Up",
        '\u{F701}' => "Down",
        '\u{F702}' => "Left",
        '\u{F703}' => "Right",
        '\u{F727}' => "Insert",
        '\u{F729}' => "Home",
        '\u{F72B}' => "End",
        '\u{F72C}' => "Page Up",
        '\u{F72D}' => "Page Down",
        '\u{F746}' => "Help",
        _ => return None,
    })
}

/// Whether a key-down goes to the main menu before the key window's first
/// responder (see [`is_menu_key`]).
pub(crate) fn goes_to_menu(event: &NSEvent) -> bool {
    let flags = event.modifierFlags();
    if flags.0 & (COMMAND | CONTROL) != 0 {
        return true;
    }
    flags.contains(NSEventModifierFlags::Function)
        && is_menu_key(flags, event.charactersIgnoringModifiers().and_then(|c| first_char(&c).0))
}

/// Whether a key-down with `flags` typing `key` goes to the main menu
/// before the key window's first responder: with Command or Control held,
/// or a function key (F1 to F35).
pub(crate) fn is_menu_key(flags: NSEventModifierFlags, key: Option<char>) -> bool {
    flags.0 & (COMMAND | CONTROL) != 0 || key.is_some_and(|k| ('\u{F704}'..='\u{F726}').contains(&k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(key: char, mask: usize) -> Shortcut {
        Shortcut { key: Key::One(key), mask, upper: lowercase(key) != key }
    }

    fn hit(s: Shortcut, key: char, flags: usize, unshifted: char) -> bool {
        matches(&s, &Pressed::new(key, flags, unshifted), || false)
    }

    #[test]
    fn exact_modifiers() {
        let q = shortcut('q', COMMAND);
        assert!(hit(q, 'q', COMMAND, 'q'));
        assert!(!hit(q, 'q', COMMAND | OPTION, 'q'));
        assert!(!hit(q, 'q', COMMAND | CONTROL, 'q'));
        assert!(!hit(q, 'q', 0, 'q'));
        // Caps Lock, Function and Numeric Pad don't count.
        let ignored = NSEventModifierFlags::CapsLock.0 | NSEventModifierFlags::Function.0;
        assert!(hit(q, 'q', COMMAND | ignored, 'q'));
    }

    #[test]
    fn shift() {
        // An uppercase key equivalent takes Shift without saying so.
        assert!(hit(shortcut('W', COMMAND), 'W', COMMAND | SHIFT, 'w'));
        assert!(!hit(shortcut('w', COMMAND), 'W', COMMAND | SHIFT, 'w'));
        // Shift in the mask: the shifted or the unshifted character.
        assert!(hit(shortcut('z', COMMAND | SHIFT), 'Z', COMMAND | SHIFT, 'z'));
        assert!(hit(shortcut('Z', COMMAND | SHIFT), 'Z', COMMAND | SHIFT, 'z'));
        assert!(!hit(shortcut('z', COMMAND | SHIFT), 'z', COMMAND, 'z'));
        // Symbols on shifted keys.
        assert!(hit(shortcut('!', COMMAND), '!', COMMAND | SHIFT, '1'));
        assert!(hit(shortcut('1', COMMAND | SHIFT), '!', COMMAND | SHIFT, '1'));
        assert!(!hit(shortcut('1', COMMAND), '!', COMMAND | SHIFT, '1'));
        // A key that Shift doesn't change: Shift must be asked for.
        assert!(!hit(shortcut('\r', 0), '\r', SHIFT, '\r'));
        assert!(hit(shortcut('\r', SHIFT), '\r', SHIFT, '\r'));
    }

    #[test]
    fn empty_never_matches() {
        let empty = Shortcut { key: Key::None, mask: COMMAND, upper: false };
        assert!(!hit(empty, 'q', COMMAND, 'q'));
    }

    #[test]
    fn us_positions() {
        assert_eq!(unshifted('!', 18), Some('1'));
        assert_eq!(unshifted('?', 44), Some('/'));
        assert_eq!(unshifted('Q', 12), Some('q'));
        assert_eq!(unshifted('\u{F704}', 122), Some('\u{F704}'));
    }

    #[test]
    fn labels() {
        let (cmd, ctrl, shift, opt) = (
            NSEventModifierFlags::Command,
            NSEventModifierFlags::Control,
            NSEventModifierFlags::Shift,
            NSEventModifierFlags::Option,
        );
        assert_eq!(label("z", ctrl | shift), "Ctrl+Shift+Z");
        assert_eq!(label("Z", ctrl), "Ctrl+Shift+Z");
        assert_eq!(label("q", cmd), "Super+Q");
        assert_eq!(label("\u{1b}", NSEventModifierFlags(0)), "Esc");
        assert_eq!(label("\r", NSEventModifierFlags(0)), "Enter");
        assert_eq!(label("\u{7f}", cmd), "Super+Backspace");
        assert_eq!(label("\u{F728}", NSEventModifierFlags(0)), "Delete");
        assert_eq!(label("\u{F704}", NSEventModifierFlags(0)), "F1");
        assert_eq!(label("\u{F70F}", opt), "Alt+F12");
        assert_eq!(label("", cmd), "");
    }

    #[test]
    fn menu_keys() {
        assert!(is_menu_key(NSEventModifierFlags::Command, Some('q')));
        assert!(is_menu_key(NSEventModifierFlags::Control, Some('k')));
        assert!(is_menu_key(NSEventModifierFlags::Function, Some('\u{F708}')));
        assert!(!is_menu_key(NSEventModifierFlags::Function, Some('\u{F700}')));
        assert!(!is_menu_key(NSEventModifierFlags(0), Some('\r')));
    }
}
