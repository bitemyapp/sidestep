//! `NSEvent` key codes: the macOS virtual key code for a Linux key.
//!
//! The table and how each value was established are in `keycode_table.rs`;
//! conformance/tests/keycodes.rs checks it against Apple's frameworks. Keys
//! macOS has no code for report [`NONE`].

include!("keycode_table.rs");

/// The key code of a key macOS has no code for. Outside the 7-bit range
/// real codes use, so it never matches a key an app is looking for.
pub(crate) const NONE: u16 = 0xFFFF;

/// macOS virtual key codes indexed by Linux evdev code.
const BY_EVDEV: [u16; 512] = {
    let mut table = [NONE; 512];
    let mut i = 0;
    while i < KEY_CODES.len() {
        table[KEY_CODES[i].0 as usize] = KEY_CODES[i].1;
        i += 1;
    }
    table
};

/// The macOS virtual key code for the key with XKB keycode `xkb` (the evdev
/// code plus 8).
pub(crate) fn mac_key_code(xkb: u16) -> u16 {
    xkb.checked_sub(8).and_then(|evdev| BY_EVDEV.get(evdev as usize)).copied().unwrap_or(NONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys() {
        // XKB keycodes are evdev codes plus 8.
        let xkb = |evdev: u16| evdev + 8;
        assert_eq!(mac_key_code(xkb(30)), 0); // A
        assert_eq!(mac_key_code(xkb(28)), 36); // Return
        assert_eq!(mac_key_code(xkb(42)), 56); // left Shift
        assert_eq!(mac_key_code(xkb(54)), 60); // right Shift
        assert_eq!(mac_key_code(xkb(59)), 122); // F1
        assert_eq!(mac_key_code(xkb(82)), 82); // keypad 0
        assert_eq!(mac_key_code(xkb(103)), 126); // Up
        assert_eq!(mac_key_code(xkb(464)), 63); // Fn
        assert_eq!(mac_key_code(xkb(116)), NONE); // Power: no macOS code
        assert_eq!(mac_key_code(3), NONE);
        assert_eq!(mac_key_code(u16::MAX), NONE);
    }

    /// The render thread reports AppKit's modifier flags and key
    /// characters (`sidestep_engine::keys`), which events pass on as they
    /// are.
    #[test]
    fn the_engine_reports_appkits_values() {
        use objc2_app_kit as ak;
        use objc2_app_kit::NSEventModifierFlags as Flags;
        use sidestep_engine::keys::{character as ch, modifier as m};

        let flags = [
            (m::CAPS_LOCK, Flags::CapsLock),
            (m::SHIFT, Flags::Shift),
            (m::CONTROL, Flags::Control),
            (m::OPTION, Flags::Option),
            (m::COMMAND, Flags::Command),
            (m::NUMERIC_PAD, Flags::NumericPad),
            (m::HELP, Flags::Help),
            (m::FUNCTION, Flags::Function),
            (m::DEVICE_INDEPENDENT, Flags::DeviceIndependentFlagsMask),
        ];
        for (ours, theirs) in flags {
            assert_eq!(ours, theirs.0, "{theirs:?}");
        }
        let characters = [
            (ch::DELETE, ak::NSDeleteCharacter),
            (ch::TAB, ak::NSTabCharacter),
            (ch::BACK_TAB, ak::NSBackTabCharacter),
            (ch::CARRIAGE_RETURN, ak::NSCarriageReturnCharacter),
            (ch::ENTER, ak::NSEnterCharacter),
            (ch::UP_ARROW, ak::NSUpArrowFunctionKey),
            (ch::DOWN_ARROW, ak::NSDownArrowFunctionKey),
            (ch::LEFT_ARROW, ak::NSLeftArrowFunctionKey),
            (ch::RIGHT_ARROW, ak::NSRightArrowFunctionKey),
            (ch::F1, ak::NSF1FunctionKey),
            (ch::F1 + 34, ak::NSF35FunctionKey),
            (ch::INSERT, ak::NSInsertFunctionKey),
            (ch::DELETE_FORWARD, ak::NSDeleteFunctionKey),
            (ch::HOME, ak::NSHomeFunctionKey),
            (ch::BEGIN, ak::NSBeginFunctionKey),
            (ch::END, ak::NSEndFunctionKey),
            (ch::PAGE_UP, ak::NSPageUpFunctionKey),
            (ch::PAGE_DOWN, ak::NSPageDownFunctionKey),
            (ch::PRINT_SCREEN, ak::NSPrintScreenFunctionKey),
            (ch::SCROLL_LOCK, ak::NSScrollLockFunctionKey),
            (ch::PAUSE, ak::NSPauseFunctionKey),
            (ch::SYS_REQ, ak::NSSysReqFunctionKey),
            (ch::BREAK, ak::NSBreakFunctionKey),
            (ch::MENU, ak::NSMenuFunctionKey),
            (ch::CLEAR_LINE, ak::NSClearLineFunctionKey),
            (ch::SELECT, ak::NSSelectFunctionKey),
            (ch::EXECUTE, ak::NSExecuteFunctionKey),
            (ch::UNDO, ak::NSUndoFunctionKey),
            (ch::REDO, ak::NSRedoFunctionKey),
            (ch::FIND, ak::NSFindFunctionKey),
            (ch::HELP, ak::NSHelpFunctionKey),
            (ch::MODE_SWITCH, ak::NSModeSwitchFunctionKey),
        ];
        for (i, (ours, theirs)) in characters.into_iter().enumerate() {
            assert_eq!(ours, theirs, "character {i}");
        }
    }
}
