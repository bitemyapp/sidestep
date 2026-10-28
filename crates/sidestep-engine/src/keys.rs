//! The modifier flags and key characters the render thread reports.
//!
//! They are AppKit's (`NSEventModifierFlags` bits, and the characters
//! `NSEvent` gives keys that type nothing: control characters and the
//! private-use function keys from U+F700), so AppKit passes them on as they
//! are; other toolkits translate them (`sidestep-ui`'s `Key`).
//! `sidestep-appkit`'s tests hold each value to objc2-app-kit's.

/// Modifier flags (`NSEventModifierFlags`), as [`Modifiers`](crate::protocol::Modifiers) holds them.
pub mod modifier {
    pub const CAPS_LOCK: usize = 1 << 16;
    pub const SHIFT: usize = 1 << 17;
    pub const CONTROL: usize = 1 << 18;
    /// Option: the Alt key.
    pub const OPTION: usize = 1 << 19;
    /// Command: the Super (logo) key.
    pub const COMMAND: usize = 1 << 20;
    /// A key of the keypad, or an arrow key.
    pub const NUMERIC_PAD: usize = 1 << 21;
    pub const HELP: usize = 1 << 22;
    /// A key whose character is a function key's (see [`super::is_function_key`]).
    pub const FUNCTION: usize = 1 << 23;
    /// The flags that don't depend on the device.
    pub const DEVICE_INDEPENDENT: usize = 0xffff_0000;
}

/// The characters keys that type nothing report.
pub mod character {
    /// Backspace (`NSDeleteCharacter`).
    pub const DELETE: u32 = 0x7f;
    pub const TAB: u32 = 0x09;
    /// Shift-Tab (`NSBackTabCharacter`).
    pub const BACK_TAB: u32 = 0x19;
    /// Return (`NSCarriageReturnCharacter`).
    pub const CARRIAGE_RETURN: u32 = 0x0d;
    /// The keypad's Enter (`NSEnterCharacter`).
    pub const ENTER: u32 = 0x03;
    pub const ESCAPE: u32 = 0x1b;

    pub const UP_ARROW: u32 = 0xF700;
    pub const DOWN_ARROW: u32 = 0xF701;
    pub const LEFT_ARROW: u32 = 0xF702;
    pub const RIGHT_ARROW: u32 = 0xF703;
    /// F1; F2 to F35 follow it.
    pub const F1: u32 = 0xF704;
    pub const INSERT: u32 = 0xF727;
    /// Forward delete (`NSDeleteFunctionKey`).
    pub const DELETE_FORWARD: u32 = 0xF728;
    pub const HOME: u32 = 0xF729;
    pub const BEGIN: u32 = 0xF72A;
    pub const END: u32 = 0xF72B;
    pub const PAGE_UP: u32 = 0xF72C;
    pub const PAGE_DOWN: u32 = 0xF72D;
    pub const PRINT_SCREEN: u32 = 0xF72E;
    pub const SCROLL_LOCK: u32 = 0xF72F;
    pub const PAUSE: u32 = 0xF730;
    pub const SYS_REQ: u32 = 0xF731;
    pub const BREAK: u32 = 0xF732;
    pub const MENU: u32 = 0xF735;
    pub const CLEAR_LINE: u32 = 0xF739;
    pub const SELECT: u32 = 0xF741;
    pub const EXECUTE: u32 = 0xF742;
    pub const UNDO: u32 = 0xF743;
    pub const REDO: u32 = 0xF744;
    pub const FIND: u32 = 0xF745;
    pub const HELP: u32 = 0xF746;
    pub const MODE_SWITCH: u32 = 0xF747;
}

/// Function keys are private-use characters from U+F700.
pub fn is_function_key(c: char) -> bool {
    ('\u{F700}'..='\u{F8FF}').contains(&c)
}
