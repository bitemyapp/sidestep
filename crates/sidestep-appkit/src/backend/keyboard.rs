//! Keys, as AppKit's key events describe them.
//!
//! The compositor sends each keyboard's keymap as XKB text and reports keys
//! by keycode and modifiers by mask. [kbvm], a pure Rust XKB
//! implementation, turns those into keysyms; this module turns keysyms into
//! what an `NSEvent` carries:
//!
//! - `characters`: what the key types with every modifier applied, Control
//!   included (Control-A is U+0001, as on macOS), after compose sequences
//!   and dead keys;
//! - `charactersIgnoringModifiers`: what it types with only Shift (and Num
//!   Lock) applied, for key equivalents;
//! - keys that type nothing get AppKit's private-use characters (the arrow
//!   keys are `NSUpArrowFunctionKey` and so on) and the control characters
//!   AppKit uses (Return is `\r`, Backspace is `NSDeleteCharacter`);
//! - modifier flags: Shift, Control, Option (Alt), Command (Super, the logo
//!   key), Caps Lock, and Function and Numeric Pad for the keys that set
//!   them on macOS.
//!
//! `keyCode` is the XKB keycode (the Linux evdev code plus 8), not the
//! value a Mac keyboard would report: those values come only from Apple's
//! headers, which the project can't use.
//!
//! [kbvm]: https://github.com/mahkoh/kbvm

use std::sync::OnceLock;

use kbvm::lookup::LookupTable;
use kbvm::xkb::Context;
use kbvm::xkb::compose::{ComposeTable, FeedResult};
use kbvm::xkb::diagnostic::{Diagnostic, DiagnosticHandler};
use kbvm::{Components, Keycode, Keysym, ModifierMask, syms};
use objc2_app_kit::NSEventModifierFlags as Flags;

use crate::protocol::Modifiers;

/// Diagnostics are for keymap authors; a client has no use for them.
struct Quiet;

impl DiagnosticHandler for Quiet {
    fn handle(&mut self, _: Diagnostic) {}
}

/// A keyboard's keymap.
pub(crate) struct Keymap {
    table: LookupTable,
    /// The modifiers AppKit calls Option and Command.
    alt: ModifierMask,
    logo: ModifierMask,
}

/// A key, translated.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Translation {
    pub characters: String,
    pub unmodified: String,
    /// A modifier key, which has no key events of its own: AppKit reports
    /// it with a flags-changed event.
    pub modifier: bool,
    pub repeats: bool,
    /// Function and Numeric Pad, which depend on the key.
    pub key_flags: Modifiers,
}

/// The compose state of one keyboard.
pub(crate) struct Compose {
    state: Option<kbvm::xkb::compose::State>,
}

/// The locale's compose table, loaded once: it's the same for every
/// keyboard, and reading it takes a few milliseconds.
fn compose_table() -> Option<&'static ComposeTable> {
    static TABLE: OnceLock<Option<ComposeTable>> = OnceLock::new();
    TABLE.get_or_init(|| Context::default().compose_table_builder().build(Quiet)).as_ref()
}

/// Start loading the compose table on a thread of its own, so the render
/// thread doesn't wait for the disk when the first keyboard appears.
pub(crate) fn load_compose_table_early() {
    let _ = std::thread::Builder::new().name("sidestep-compose".into()).spawn(|| {
        compose_table();
    });
}

impl Compose {
    pub fn new() -> Self {
        Compose { state: compose_table().map(|t| t.create_state()) }
    }

    /// Feed a keysym; `None` if it isn't part of a sequence, else what the
    /// key types (empty while a sequence is pending).
    fn feed(&mut self, sym: Keysym) -> Option<String> {
        let (table, state) = (compose_table()?, self.state.as_mut()?);
        match table.feed(state, sym)? {
            FeedResult::Pending => Some(String::new()),
            FeedResult::Composed { string, keysym } => Some(
                string
                    .map(str::to_owned)
                    .or_else(|| keysym.and_then(Keysym::char).map(String::from))
                    .unwrap_or_default(),
            ),
            // The key that broke the sequence types itself.
            FeedResult::Aborted => None,
        }
    }

    pub fn reset(&mut self) {
        if let (Some(table), Some(state)) = (compose_table(), self.state.as_mut()) {
            *state = table.create_state();
        }
    }
}

impl Keymap {
    /// Parse the keymap text a compositor sends (its trailing NUL
    /// included or not).
    pub fn parse(text: &[u8]) -> Option<Keymap> {
        let text = text.strip_suffix(&[0]).unwrap_or(text);
        let keymap = Context::default().keymap_from_bytes(Quiet, None, text).ok()?;
        let named = |names: &[&str]| {
            keymap
                .virtual_modifiers()
                .filter(|m| names.contains(&m.name()))
                .fold(ModifierMask::NONE, |acc, m| acc | m.mask())
        };
        let or = |mask: ModifierMask, default: ModifierMask| if mask == ModifierMask::NONE { default } else { mask };
        Some(Keymap {
            table: keymap.to_builder().build_lookup_table(),
            alt: or(named(&["Alt"]), ModifierMask::ALT),
            logo: or(named(&["Super"]), ModifierMask::SUPER),
        })
    }

    /// The modifier flags of a keyboard in state `c`.
    pub fn flags(&self, c: &Components) -> Modifiers {
        let held = c.mods_pressed | c.mods_latched;
        let mut flags = Flags(0);
        let mut set = |mask: ModifierMask, flag: Flags| {
            if (held & mask) != ModifierMask::NONE {
                flags |= flag;
            }
        };
        set(ModifierMask::SHIFT, Flags::Shift);
        set(ModifierMask::CONTROL, Flags::Control);
        set(self.alt, Flags::Option);
        set(self.logo, Flags::Command);
        if (c.mods & ModifierMask::LOCK) != ModifierMask::NONE {
            flags |= Flags::CapsLock;
        }
        flags.0
    }

    /// Translate the key with XKB keycode `code` in state `c`, feeding the
    /// compose state if there is one (presses only).
    pub fn translate(&self, c: &Components, code: u32, compose: Option<&mut Compose>) -> Translation {
        let key = Keycode::from_x11(code);
        let lookup = self.table.lookup(c.group, c.mods, key);
        let repeats = lookup.repeats();
        let props: Vec<_> = lookup.into_iter().collect();
        let Some(first) = props.first() else {
            return Translation { repeats, ..Default::default() };
        };
        let sym = first.keysym();
        if sym.is_modifier() {
            return Translation { modifier: true, ..Default::default() };
        }
        let mut key_flags = Flags(0);
        if sym.is_keypad() || is_arrow(sym) {
            key_flags |= Flags::NumericPad;
        }

        let characters = match special(sym) {
            Some(ch) => ch.to_string(),
            None => {
                let typed: String = props.iter().filter_map(|p| p.char()).collect();
                // Shortcuts don't compose: only plain and shifted keys do.
                let shortcut = (c.mods & (ModifierMask::CONTROL | self.alt | self.logo)) != ModifierMask::NONE;
                match compose.filter(|_| !shortcut).and_then(|comp| comp.feed(sym)) {
                    Some(composed) => composed,
                    None => typed,
                }
            }
        };

        // As typed with Shift and Num Lock only: no Control characters and
        // no third level.
        let plain = c.mods & (ModifierMask::SHIFT | ModifierMask::NUM_LOCK);
        let unmodified_lookup = self.table.lookup(c.group, plain, key).with_ctrl_transform(false);
        let unmodified: String = unmodified_lookup
            .into_iter()
            .map(|p| special(p.keysym()).or_else(|| p.char()))
            .take_while(Option::is_some)
            .flatten()
            .collect();

        if characters.chars().next().is_some_and(is_function_key) {
            key_flags |= Flags::Function;
        }
        Translation { characters, unmodified, modifier: false, repeats, key_flags: key_flags.0 }
    }
}

fn is_arrow(sym: Keysym) -> bool {
    matches!(sym, syms::Left | syms::Right | syms::Up | syms::Down)
}

/// AppKit's function keys are private-use characters from U+F700.
fn is_function_key(c: char) -> bool {
    ('\u{F700}'..='\u{F8FF}').contains(&c)
}

/// The characters AppKit reports for keys that don't type text.
fn special(sym: Keysym) -> Option<char> {
    use objc2_app_kit as ak;
    let code: u32 = match sym {
        syms::BackSpace => ak::NSDeleteCharacter,
        syms::Tab | syms::KP_Tab => ak::NSTabCharacter,
        syms::ISO_Left_Tab => ak::NSBackTabCharacter,
        syms::Return => ak::NSCarriageReturnCharacter,
        syms::KP_Enter => ak::NSEnterCharacter,
        syms::Escape => 0x1b,
        syms::Delete | syms::KP_Delete => ak::NSDeleteFunctionKey,
        syms::Up | syms::KP_Up => ak::NSUpArrowFunctionKey,
        syms::Down | syms::KP_Down => ak::NSDownArrowFunctionKey,
        syms::Left | syms::KP_Left => ak::NSLeftArrowFunctionKey,
        syms::Right | syms::KP_Right => ak::NSRightArrowFunctionKey,
        syms::Home | syms::KP_Home => ak::NSHomeFunctionKey,
        syms::End | syms::KP_End => ak::NSEndFunctionKey,
        syms::Prior | syms::KP_Prior => ak::NSPageUpFunctionKey,
        syms::Next | syms::KP_Next => ak::NSPageDownFunctionKey,
        syms::Begin | syms::KP_Begin => ak::NSBeginFunctionKey,
        syms::Insert | syms::KP_Insert => ak::NSInsertFunctionKey,
        syms::Clear => ak::NSClearLineFunctionKey,
        syms::Pause => ak::NSPauseFunctionKey,
        syms::Scroll_Lock => ak::NSScrollLockFunctionKey,
        syms::Sys_Req => ak::NSSysReqFunctionKey,
        syms::Break => ak::NSBreakFunctionKey,
        syms::Print => ak::NSPrintScreenFunctionKey,
        syms::Select => ak::NSSelectFunctionKey,
        syms::Execute => ak::NSExecuteFunctionKey,
        syms::Undo => ak::NSUndoFunctionKey,
        syms::Redo => ak::NSRedoFunctionKey,
        syms::Menu => ak::NSMenuFunctionKey,
        syms::Find => ak::NSFindFunctionKey,
        syms::Help => ak::NSHelpFunctionKey,
        syms::Mode_switch => ak::NSModeSwitchFunctionKey,
        s if (syms::F1..=syms::F35).contains(&s) => ak::NSF1FunctionKey + (s.0 - syms::F1.0),
        _ => return None,
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use kbvm::evdev;
    use kbvm::xkb::rmlvo::Group;

    use super::*;

    /// A US keymap from the system's XKB data, as a compositor would send.
    fn us() -> Keymap {
        let context = Context::default();
        let groups: Vec<_> = Group::from_layouts_and_variants("us", "").collect();
        let keymap = context.keymap_from_names(Quiet, None, None, Some(&groups), None);
        Keymap::parse(keymap.format().to_string().as_bytes()).expect("the keymap parses")
    }

    fn state(mods: ModifierMask) -> Components {
        let mut c = Components::default();
        c.mods_pressed = mods;
        c.update_effective();
        c
    }

    fn xkb(evdev: Keycode) -> u32 {
        evdev.to_x11()
    }

    #[test]
    fn letters_and_shift() {
        let map = us();
        let t = map.translate(&state(ModifierMask::NONE), xkb(evdev::A), None);
        assert_eq!((t.characters.as_str(), t.unmodified.as_str()), ("a", "a"));
        assert!(t.repeats && !t.modifier);
        let t = map.translate(&state(ModifierMask::SHIFT), xkb(evdev::A), None);
        assert_eq!((t.characters.as_str(), t.unmodified.as_str()), ("A", "A"));
        let t = map.translate(&state(ModifierMask::SHIFT), xkb(evdev::_1), None);
        assert_eq!((t.characters.as_str(), t.unmodified.as_str()), ("!", "!"));
    }

    #[test]
    fn control_and_command() {
        let map = us();
        let t = map.translate(&state(ModifierMask::CONTROL), xkb(evdev::A), None);
        assert_eq!((t.characters.as_str(), t.unmodified.as_str()), ("\u{1}", "a"));
        let t = map.translate(&state(ModifierMask::SUPER), xkb(evdev::A), None);
        assert_eq!((t.characters.as_str(), t.unmodified.as_str()), ("a", "a"));
        let flags = map.flags(&state(ModifierMask::SUPER | ModifierMask::ALT | ModifierMask::SHIFT));
        assert_eq!(flags, (Flags::Command | Flags::Option | Flags::Shift).0);
        let flags = map.flags(&state(ModifierMask::CONTROL));
        assert_eq!(flags, Flags::Control.0);
    }

    #[test]
    fn caps_lock_is_a_locked_flag() {
        let map = us();
        let mut c = Components::default();
        c.mods_locked = ModifierMask::LOCK;
        c.update_effective();
        assert_eq!(map.flags(&c), Flags::CapsLock.0);
        let t = map.translate(&c, xkb(evdev::A), None);
        assert_eq!(t.characters, "A");
    }

    #[test]
    fn keys_that_type_nothing() {
        let map = us();
        let none = state(ModifierMask::NONE);
        let chars = |code| map.translate(&none, xkb(code), None).characters;
        assert_eq!(chars(evdev::ENTER), "\r");
        assert_eq!(chars(evdev::KPENTER), "\u{3}");
        assert_eq!(chars(evdev::BACKSPACE), "\u{7f}");
        assert_eq!(chars(evdev::TAB), "\t");
        assert_eq!(chars(evdev::ESC), "\u{1b}");
        assert_eq!(chars(evdev::DELETE), "\u{F728}");
        assert_eq!(chars(evdev::UP), "\u{F700}");
        assert_eq!(chars(evdev::F1), "\u{F704}");
        assert_eq!(chars(evdev::F12), "\u{F70F}");
        let shift_tab = map.translate(&state(ModifierMask::SHIFT), xkb(evdev::TAB), None);
        assert_eq!(shift_tab.characters, "\u{19}");
        let left = map.translate(&none, xkb(evdev::LEFT), None);
        assert_eq!(left.key_flags, (Flags::Function | Flags::NumericPad).0);
        let a = map.translate(&none, xkb(evdev::A), None);
        assert_eq!(a.key_flags, 0);
    }

    #[test]
    fn modifier_keys_are_flags_only() {
        let map = us();
        for code in [evdev::LEFTSHIFT, evdev::RIGHTCTRL, evdev::LEFTALT, evdev::LEFTMETA, evdev::CAPSLOCK] {
            let t = map.translate(&state(ModifierMask::NONE), xkb(code), None);
            assert!(t.modifier, "{code:?}");
            assert!(t.characters.is_empty());
        }
    }

    #[test]
    fn keypad_follows_num_lock() {
        let map = us();
        let with = map.translate(&state(ModifierMask::NUM_LOCK), xkb(evdev::KP1), None);
        assert_eq!(with.characters, "1");
        assert_ne!(with.key_flags & Flags::NumericPad.0, 0);
        let without = map.translate(&state(ModifierMask::NONE), xkb(evdev::KP1), None);
        assert_eq!(without.characters, "\u{F72B}");
    }

    /// `cargo test --release -p sidestep-appkit keyboard_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn keyboard_timing() {
        let context = Context::default();
        let groups: Vec<_> = Group::from_layouts_and_variants("us", "").collect();
        let text = context.keymap_from_names(Quiet, None, None, Some(&groups), None).format().to_string();
        let parse = super::super::median(|| drop(Keymap::parse(text.as_bytes())));
        let compose_load = super::super::median(|| drop(Context::default().compose_table_builder().build(Quiet)));
        println!("the locale's compose table loads in {compose_load:.2} ms");
        let map = us();
        let shifted = state(ModifierMask::SHIFT);
        let keys = [evdev::A, evdev::_1, evdev::ENTER, evdev::LEFT, evdev::F1, evdev::SPACE, evdev::Q, evdev::KP1];
        let mut compose = Compose::new();
        let per_key = super::super::median(|| {
            for _ in 0..1000 {
                for key in keys {
                    std::hint::black_box(map.translate(&shifted, xkb(key), Some(&mut compose)));
                }
            }
        }) * 1e6
            / (1000 * keys.len()) as f64;
        println!("US keymap: parsed in {parse:.2} ms; a key translates in {per_key:.0} ns");
    }

    #[test]
    fn dead_keys_compose() {
        // US international: the apostrophe key is a dead acute.
        let context = Context::default();
        let groups: Vec<_> = Group::from_layouts_and_variants("us", "intl").collect();
        let keymap = context.keymap_from_names(Quiet, None, None, Some(&groups), None);
        let map = Keymap::parse(keymap.format().to_string().as_bytes()).unwrap();
        let Some(_) = compose_table() else {
            // No compose data on this system; nothing to check.
            return;
        };
        let mut compose = Compose::new();
        let none = state(ModifierMask::NONE);
        let dead = map.translate(&none, xkb(evdev::APOSTROPHE), Some(&mut compose));
        assert_eq!(dead.characters, "");
        let e = map.translate(&none, xkb(evdev::E), Some(&mut compose));
        assert_eq!(e.characters, "é");
        assert_eq!(e.unmodified, "e");
        let plain = map.translate(&none, xkb(evdev::E), Some(&mut compose));
        assert_eq!(plain.characters, "e");
    }
}
