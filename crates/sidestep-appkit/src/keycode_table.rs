// macOS virtual key codes for Linux keys. Included by conformance/tests/
// keycodes.rs, and by AppKit's keyboard handling to give NSEvent its
// keyCode, so it holds only data.
//
// NSEvent's keyCode is Apple's virtual key code, which names a key by its
// position on the keyboard. Each row maps a Linux evdev code (the XKB
// keycode minus 8) to one, and records what macOS reports for that code, so
// conformance/tests/keycodes.rs can check every row against Apple's
// frameworks through public API:
// - Char: what the US layout types for the key (UCKeyTranslate), for keys
//   outside the numeric keypad;
// - Keypad: the same, for keys macOS flags as part of the numeric keypad;
// - Function: the NSEvent function-key character (NSUpArrowFunctionKey and
//   so on);
// - Modifier: the flags of the flags-changed event macOS makes for the key,
//   including the device bit that tells left from right;
// - Conventional: keys that neither the US layout nor a synthetic event
//   identifies (volume, F20, the JIS keys, the context-menu key, and the PC
//   keys macOS treats as F13 to F15, which therefore share their codes).

/// What macOS reports for a virtual key code.
#[allow(dead_code)]
pub enum Seen {
    Char(&'static str),
    Keypad(&'static str),
    Function(u16),
    Modifier(u64),
    Conventional,
}

/// (evdev code, macOS virtual key code, what macOS reports for it), sorted
/// by evdev code.
pub const KEY_CODES: &[(u16, u16, Seen)] = &[
    (1, 53, Seen::Char("\u{1b}")), // KEY_ESC
    (2, 18, Seen::Char("1")), // KEY_1
    (3, 19, Seen::Char("2")), // KEY_2
    (4, 20, Seen::Char("3")), // KEY_3
    (5, 21, Seen::Char("4")), // KEY_4
    (6, 23, Seen::Char("5")), // KEY_5
    (7, 22, Seen::Char("6")), // KEY_6
    (8, 26, Seen::Char("7")), // KEY_7
    (9, 28, Seen::Char("8")), // KEY_8
    (10, 25, Seen::Char("9")), // KEY_9
    (11, 29, Seen::Char("0")), // KEY_0
    (12, 27, Seen::Char("-")), // KEY_MINUS
    (13, 24, Seen::Char("=")), // KEY_EQUAL
    (14, 51, Seen::Char("\u{8}")), // KEY_BACKSPACE
    (15, 48, Seen::Char("\t")), // KEY_TAB
    (16, 12, Seen::Char("q")), // KEY_Q
    (17, 13, Seen::Char("w")), // KEY_W
    (18, 14, Seen::Char("e")), // KEY_E
    (19, 15, Seen::Char("r")), // KEY_R
    (20, 17, Seen::Char("t")), // KEY_T
    (21, 16, Seen::Char("y")), // KEY_Y
    (22, 32, Seen::Char("u")), // KEY_U
    (23, 34, Seen::Char("i")), // KEY_I
    (24, 31, Seen::Char("o")), // KEY_O
    (25, 35, Seen::Char("p")), // KEY_P
    (26, 33, Seen::Char("[")), // KEY_LEFTBRACE
    (27, 30, Seen::Char("]")), // KEY_RIGHTBRACE
    (28, 36, Seen::Char("\r")), // KEY_ENTER
    (29, 59, Seen::Modifier(0x40001)), // KEY_LEFTCTRL
    (30, 0, Seen::Char("a")), // KEY_A
    (31, 1, Seen::Char("s")), // KEY_S
    (32, 2, Seen::Char("d")), // KEY_D
    (33, 3, Seen::Char("f")), // KEY_F
    (34, 5, Seen::Char("g")), // KEY_G
    (35, 4, Seen::Char("h")), // KEY_H
    (36, 38, Seen::Char("j")), // KEY_J
    (37, 40, Seen::Char("k")), // KEY_K
    (38, 37, Seen::Char("l")), // KEY_L
    (39, 41, Seen::Char(";")), // KEY_SEMICOLON
    (40, 39, Seen::Char("'")), // KEY_APOSTROPHE
    (41, 50, Seen::Char("`")), // KEY_GRAVE
    (42, 56, Seen::Modifier(0x20002)), // KEY_LEFTSHIFT
    (43, 42, Seen::Char("\\")), // KEY_BACKSLASH
    (44, 6, Seen::Char("z")), // KEY_Z
    (45, 7, Seen::Char("x")), // KEY_X
    (46, 8, Seen::Char("c")), // KEY_C
    (47, 9, Seen::Char("v")), // KEY_V
    (48, 11, Seen::Char("b")), // KEY_B
    (49, 45, Seen::Char("n")), // KEY_N
    (50, 46, Seen::Char("m")), // KEY_M
    (51, 43, Seen::Char(",")), // KEY_COMMA
    (52, 47, Seen::Char(".")), // KEY_DOT
    (53, 44, Seen::Char("/")), // KEY_SLASH
    (54, 60, Seen::Modifier(0x20004)), // KEY_RIGHTSHIFT
    (55, 67, Seen::Keypad("*")), // KEY_KPASTERISK
    (56, 58, Seen::Modifier(0x80020)), // KEY_LEFTALT
    (57, 49, Seen::Char(" ")), // KEY_SPACE
    (58, 57, Seen::Modifier(0x10000)), // KEY_CAPSLOCK
    (59, 122, Seen::Function(0xF704)), // KEY_F1
    (60, 120, Seen::Function(0xF705)), // KEY_F2
    (61, 99, Seen::Function(0xF706)), // KEY_F3
    (62, 118, Seen::Function(0xF707)), // KEY_F4
    (63, 96, Seen::Function(0xF708)), // KEY_F5
    (64, 97, Seen::Function(0xF709)), // KEY_F6
    (65, 98, Seen::Function(0xF70A)), // KEY_F7
    (66, 100, Seen::Function(0xF70B)), // KEY_F8
    (67, 101, Seen::Function(0xF70C)), // KEY_F9
    (68, 109, Seen::Function(0xF70D)), // KEY_F10
    (69, 71, Seen::Function(0xF739)), // KEY_NUMLOCK
    (70, 107, Seen::Conventional), // KEY_SCROLLLOCK
    (71, 89, Seen::Keypad("7")), // KEY_KP7
    (72, 91, Seen::Keypad("8")), // KEY_KP8
    (73, 92, Seen::Keypad("9")), // KEY_KP9
    (74, 78, Seen::Keypad("-")), // KEY_KPMINUS
    (75, 86, Seen::Keypad("4")), // KEY_KP4
    (76, 87, Seen::Keypad("5")), // KEY_KP5
    (77, 88, Seen::Keypad("6")), // KEY_KP6
    (78, 69, Seen::Keypad("+")), // KEY_KPPLUS
    (79, 83, Seen::Keypad("1")), // KEY_KP1
    (80, 84, Seen::Keypad("2")), // KEY_KP2
    (81, 85, Seen::Keypad("3")), // KEY_KP3
    (82, 82, Seen::Keypad("0")), // KEY_KP0
    (83, 65, Seen::Keypad(".")), // KEY_KPDOT
    (86, 10, Seen::Char("\u{a7}")), // KEY_102ND
    (87, 103, Seen::Function(0xF70E)), // KEY_F11
    (88, 111, Seen::Function(0xF70F)), // KEY_F12
    (89, 94, Seen::Conventional), // KEY_RO
    (96, 76, Seen::Keypad("\u{3}")), // KEY_KPENTER
    (97, 62, Seen::Modifier(0x42000)), // KEY_RIGHTCTRL
    (98, 75, Seen::Keypad("/")), // KEY_KPSLASH
    (99, 105, Seen::Conventional), // KEY_SYSRQ
    (100, 61, Seen::Modifier(0x80040)), // KEY_RIGHTALT
    (102, 115, Seen::Function(0xF729)), // KEY_HOME
    (103, 126, Seen::Function(0xF700)), // KEY_UP
    (104, 116, Seen::Function(0xF72C)), // KEY_PAGEUP
    (105, 123, Seen::Function(0xF702)), // KEY_LEFT
    (106, 124, Seen::Function(0xF703)), // KEY_RIGHT
    (107, 119, Seen::Function(0xF72B)), // KEY_END
    (108, 125, Seen::Function(0xF701)), // KEY_DOWN
    (109, 121, Seen::Function(0xF72D)), // KEY_PAGEDOWN
    (110, 114, Seen::Function(0xF746)), // KEY_INSERT
    (111, 117, Seen::Function(0xF728)), // KEY_DELETE
    (113, 74, Seen::Conventional), // KEY_MUTE
    (114, 73, Seen::Conventional), // KEY_VOLUMEDOWN
    (115, 72, Seen::Conventional), // KEY_VOLUMEUP
    (117, 81, Seen::Keypad("=")), // KEY_KPEQUAL
    (119, 113, Seen::Conventional), // KEY_PAUSE
    (121, 95, Seen::Conventional), // KEY_KPCOMMA
    (122, 104, Seen::Conventional), // KEY_HANGEUL
    (123, 102, Seen::Conventional), // KEY_HANJA
    (124, 93, Seen::Conventional), // KEY_YEN
    (125, 55, Seen::Modifier(0x100008)), // KEY_LEFTMETA
    (126, 54, Seen::Modifier(0x100010)), // KEY_RIGHTMETA
    (127, 110, Seen::Conventional), // KEY_COMPOSE
    (183, 105, Seen::Function(0xF710)), // KEY_F13
    (184, 107, Seen::Function(0xF711)), // KEY_F14
    (185, 113, Seen::Function(0xF712)), // KEY_F15
    (186, 106, Seen::Function(0xF713)), // KEY_F16
    (187, 64, Seen::Function(0xF714)), // KEY_F17
    (188, 79, Seen::Function(0xF715)), // KEY_F18
    (189, 80, Seen::Function(0xF716)), // KEY_F19
    (190, 90, Seen::Conventional), // KEY_F20
    (464, 63, Seen::Modifier(0x800000)), // KEY_FN
];
