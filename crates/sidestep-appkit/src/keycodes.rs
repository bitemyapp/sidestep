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
}
