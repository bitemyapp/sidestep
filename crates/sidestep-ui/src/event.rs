//! What a window hears: input, and what the compositor did to it.

use kurbo::{Point, Size, Vec2};
use sidestep_engine::keys::{character as ch, is_function_key, modifier};
use sidestep_engine::protocol;

/// What happened to a window or in it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum WindowEvent {
    /// The compositor gave the window's content a new size, in points (the
    /// first time, its first size). The window draws itself whole next.
    Resized(Size),
    /// The window shows at a new scale: device pixels per point (the first
    /// time, its first scale). It may be fractional.
    ScaleChanged(f64),
    /// The compositor maximized, tiled, suspended (…) the window, or undid
    /// it.
    StateChanged(WindowState),
    /// The user asked to close the window (its close button, a shortcut of
    /// the desktop's). It stays open unless the handler closes it.
    CloseRequested,
    /// The window got or lost the keyboard.
    Focused(bool),
    Key(KeyEvent),
    /// The modifier keys held changed.
    ModifiersChanged(Modifiers),
    /// The pointer came over the window's content.
    PointerEntered(Point),
    PointerMoved {
        position: Point,
        modifiers: Modifiers,
    },
    PointerLeft,
    PointerButton {
        position: Point,
        button: PointerButton,
        pressed: bool,
        /// 1 for a single click, 2 for a double click, and so on.
        clicks: u32,
        modifiers: Modifiers,
        /// The press that gave the window the keyboard: programs often
        /// let it only activate the window.
        activating: bool,
    },
    Scroll {
        position: Point,
        delta: ScrollDelta,
        /// Where a touchpad gesture is; [`ScrollPhase::None`] for wheels.
        phase: ScrollPhase,
        /// At a touchpad gesture's end, the fingers' speed in points per
        /// second, for scrolling to coast on.
        velocity: Vec2,
        /// The compositor reverses the device's direction (natural
        /// scrolling); `delta` is already the way content should move.
        inverted: bool,
        modifiers: Modifiers,
    },
    /// A two-finger pinch on a touchpad.
    Pinch {
        position: Point,
        phase: ScrollPhase,
        /// The change of scale since the last update, as a factor less
        /// one.
        magnification: f64,
        /// The change of rotation since the last update, in degrees
        /// counterclockwise.
        rotation: f64,
        modifiers: Modifiers,
    },
    /// What an input method did, while the window takes text from one
    /// (`Window::set_text_input`).
    Ime(Ime),
    /// The compositor dismissed the popup (a click elsewhere): it is
    /// closed.
    PopupDismissed,
}

/// The xdg_toplevel states a window can be in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct WindowState {
    pub maximized: bool,
    pub fullscreen: bool,
    /// The desktop shows the window as the active one.
    pub activated: bool,
    /// Tiled against other windows or the screen's edges.
    pub tiled: bool,
    /// The compositor isn't showing the window (behind others, minimized,
    /// on another workspace).
    pub suspended: bool,
    /// The user is resizing the window.
    pub resizing: bool,
}

impl WindowState {
    pub(crate) fn from_protocol(s: protocol::WindowState) -> WindowState {
        WindowState {
            maximized: s.maximized,
            fullscreen: s.fullscreen,
            activated: s.activated,
            tiled: s.tiled,
            suspended: s.suspended,
            resizing: s.resizing,
        }
    }
}

/// Modifier keys held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Modifiers = Modifiers(0);
    pub const SHIFT: Modifiers = Modifiers(1);
    pub const CONTROL: Modifiers = Modifiers(1 << 1);
    pub const ALT: Modifiers = Modifiers(1 << 2);
    /// The Super (logo) key.
    pub const SUPER: Modifiers = Modifiers(1 << 3);
    /// Caps Lock is on.
    pub const CAPS_LOCK: Modifiers = Modifiers(1 << 4);

    pub fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn shift(self) -> bool {
        self.contains(Modifiers::SHIFT)
    }

    pub fn control(self) -> bool {
        self.contains(Modifiers::CONTROL)
    }

    pub fn alt(self) -> bool {
        self.contains(Modifiers::ALT)
    }

    pub fn super_key(self) -> bool {
        self.contains(Modifiers::SUPER)
    }

    /// The modifiers as the render thread reports them
    /// (`sidestep_engine::keys::modifier` bits).
    pub(crate) fn from_flags(flags: protocol::Modifiers) -> Modifiers {
        let mut m = Modifiers::NONE;
        for (bit, ours) in [
            (modifier::SHIFT, Modifiers::SHIFT),
            (modifier::CONTROL, Modifiers::CONTROL),
            (modifier::OPTION, Modifiers::ALT),
            (modifier::COMMAND, Modifiers::SUPER),
            (modifier::CAPS_LOCK, Modifiers::CAPS_LOCK),
        ] {
            if flags & bit != 0 {
                m |= ours;
            }
        }
        m
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Modifiers;

    fn bitor(self, other: Modifiers) -> Modifiers {
        Modifiers(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for Modifiers {
    fn bitor_assign(&mut self, other: Modifiers) {
        self.0 |= other.0;
    }
}

/// A key pressed, repeated or released, through the keyboard's layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    /// The text the key types, if it types any: none for keys that type
    /// nothing (arrows, Escape), for releases, and while Control or Super
    /// is held (shortcuts, which `key` and `modifiers` tell apart). Dead
    /// keys and compose sequences type their result on the key that ends
    /// them.
    pub text: Option<String>,
    pub pressed: bool,
    /// A press repeated while the key is held.
    pub repeat: bool,
    /// The physical key: its Linux evdev code (`KEY_A` is 30), the same
    /// whatever the layout.
    pub code: u32,
    pub modifiers: Modifiers,
    /// A compose sequence or dead key as it stands after this key, when it
    /// started, continued or ended one (empty once it's over): what a text
    /// field shows as being composed.
    pub composing: Option<String>,
}

/// What a key is, through the keyboard's layout.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A key that types text: what it types with Shift alone applied (`a`,
    /// `A` with Shift, `1`, `!`, ` `), whatever else is held.
    Character(String),
    Named(NamedKey),
    /// A key the layout gives nothing.
    Unidentified,
}

/// Keys that type no text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NamedKey {
    Enter,
    Tab,
    Backspace,
    Escape,
    /// Forward delete.
    Delete,
    Insert,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Begin,
    /// F1 to F35.
    F(u8),
    PrintScreen,
    ScrollLock,
    Pause,
    SysReq,
    Break,
    Menu,
    Clear,
    Select,
    Execute,
    Undo,
    Redo,
    Find,
    Help,
    ModeSwitch,
}

impl NamedKey {
    /// The key AppKit's character `c` stands for, if it stands for one.
    fn of(c: char) -> Option<NamedKey> {
        use NamedKey::*;
        let c = u32::from(c);
        Some(match c {
            ch::CARRIAGE_RETURN | ch::ENTER | 0x0a => Enter,
            ch::TAB | ch::BACK_TAB => Tab,
            ch::DELETE | 0x08 => Backspace,
            ch::ESCAPE => Escape,
            ch::DELETE_FORWARD => Delete,
            ch::INSERT => Insert,
            ch::UP_ARROW => ArrowUp,
            ch::DOWN_ARROW => ArrowDown,
            ch::LEFT_ARROW => ArrowLeft,
            ch::RIGHT_ARROW => ArrowRight,
            ch::HOME => Home,
            ch::END => End,
            ch::PAGE_UP => PageUp,
            ch::PAGE_DOWN => PageDown,
            ch::BEGIN => Begin,
            f if (ch::F1..ch::F1 + 35).contains(&f) => F((f - ch::F1 + 1) as u8),
            ch::PRINT_SCREEN => PrintScreen,
            ch::SCROLL_LOCK => ScrollLock,
            ch::PAUSE => Pause,
            ch::SYS_REQ => SysReq,
            ch::BREAK => Break,
            ch::MENU => Menu,
            ch::CLEAR_LINE => Clear,
            ch::SELECT => Select,
            ch::EXECUTE => Execute,
            ch::UNDO => Undo,
            ch::REDO => Redo,
            ch::FIND => Find,
            ch::HELP => Help,
            ch::MODE_SWITCH => ModeSwitch,
            _ => return None,
        })
    }
}

impl KeyEvent {
    pub(crate) fn from_protocol(key: protocol::Key) -> KeyEvent {
        let modifiers = Modifiers::from_flags(key.modifiers);
        let named = key.unmodified.chars().next().or_else(|| key.characters.chars().next()).and_then(NamedKey::of);
        let key_of = match named {
            Some(n) => Key::Named(n),
            None if !key.unmodified.is_empty() => Key::Character(key.unmodified.clone()),
            None if !key.characters.is_empty() => Key::Character(key.characters.clone()),
            None => Key::Unidentified,
        };
        let types = |s: &str| !s.is_empty() && s.chars().all(|c| !c.is_control() && !is_function_key(c));
        let shortcut = modifiers.control() || modifiers.super_key();
        let text = (key.down && named.is_none() && !shortcut && types(&key.characters)).then_some(key.characters);
        KeyEvent {
            key: key_of,
            text,
            pressed: key.down,
            repeat: key.repeat,
            code: u32::from(key.code).saturating_sub(8),
            modifiers,
            composing: key.composing,
        }
    }
}

/// A pointer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    /// Another button, numbered from 5 on.
    Other(u8),
}

impl PointerButton {
    pub(crate) fn from_protocol(b: protocol::Button) -> PointerButton {
        match b {
            protocol::Button::Left => PointerButton::Left,
            protocol::Button::Right => PointerButton::Right,
            protocol::Button::Other(2) => PointerButton::Middle,
            protocol::Button::Other(3) => PointerButton::Back,
            protocol::Button::Other(4) => PointerButton::Forward,
            protocol::Button::Other(n) => PointerButton::Other(n),
        }
    }
}

/// How far to scroll: positive toward the content's right and bottom (the
/// content moves up and left), as Wayland counts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollDelta {
    /// Points, from a touchpad or another continuous source.
    Points(Vec2),
    /// Lines (wheel detents; fractions from high-resolution wheels).
    Lines(Vec2),
}

/// Where a touchpad gesture is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScrollPhase {
    /// Not part of a gesture: a wheel, say.
    None,
    Began,
    Changed,
    /// The fingers lifted.
    Ended,
}

impl ScrollPhase {
    pub(crate) fn from_protocol(p: protocol::ScrollPhase) -> ScrollPhase {
        match p {
            protocol::ScrollPhase::None => ScrollPhase::None,
            protocol::ScrollPhase::Began => ScrollPhase::Began,
            protocol::ScrollPhase::Changed => ScrollPhase::Changed,
            protocol::ScrollPhase::Ended => ScrollPhase::Ended,
        }
    }
}

/// What an input method did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ime {
    /// Text to insert where the caret is, replacing what was being
    /// composed.
    Commit(String),
    /// The text being composed (empty when composing ends), with its
    /// selection or caret as a byte range of it, if it shows one.
    Preedit { text: String, selection: Option<std::ops::Range<usize>> },
}

impl Ime {
    /// An input method's changes, as the render thread sends them: the text
    /// to insert, then what is being composed.
    pub(crate) fn from_protocol(commit: Option<String>, (text, begin, end): (String, i32, i32)) -> Vec<Ime> {
        let mut out: Vec<Ime> = commit.map(Ime::Commit).into_iter().collect();
        let at = |i: i32| usize::try_from(i).ok().filter(|i| *i <= text.len() && text.is_char_boundary(*i));
        let selection = at(begin).zip(at(end)).map(|(a, b)| a.min(b)..a.max(b));
        out.push(Ime::Preedit { text, selection });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(characters: &str, unmodified: &str, modifiers: usize) -> KeyEvent {
        KeyEvent::from_protocol(protocol::Key {
            down: true,
            repeat: false,
            code: 38,
            characters: characters.into(),
            unmodified: unmodified.into(),
            modifiers,
            composing: None,
        })
    }

    #[test]
    fn letters_type_text_and_shortcuts_dont() {
        let a = key("a", "a", 0);
        assert_eq!((a.key, a.text, a.code), (Key::Character("a".into()), Some("a".into()), 30));
        let shifted = key("A", "A", modifier::SHIFT);
        assert_eq!(shifted.text.as_deref(), Some("A"));
        assert!(shifted.modifiers.shift());
        // Control-A types U+0001 through the layout: a shortcut, no text.
        let control = key("\u{1}", "a", modifier::CONTROL);
        assert_eq!((control.key, control.text), (Key::Character("a".into()), None));
        let super_a = key("a", "a", modifier::COMMAND);
        assert_eq!(super_a.text, None);
        assert!(super_a.modifiers.super_key());
    }

    #[test]
    fn keys_that_type_nothing_are_named() {
        let up = key("\u{F700}", "\u{F700}", modifier::FUNCTION | modifier::NUMERIC_PAD);
        assert_eq!((up.key, up.text), (Key::Named(NamedKey::ArrowUp), None));
        assert_eq!(key("\r", "\r", 0).key, Key::Named(NamedKey::Enter));
        assert_eq!(key("\u{7f}", "\u{7f}", 0).key, Key::Named(NamedKey::Backspace));
        assert_eq!(key("\u{19}", "\u{9}", modifier::SHIFT).key, Key::Named(NamedKey::Tab));
        assert_eq!(key("\u{1b}", "\u{1b}", 0).key, Key::Named(NamedKey::Escape));
        assert_eq!(key("\u{F704}", "\u{F704}", 0).key, Key::Named(NamedKey::F(1)));
        assert_eq!(key("\u{F726}", "\u{F726}", 0).key, Key::Named(NamedKey::F(35)));
        assert_eq!(key(" ", " ", 0).text.as_deref(), Some(" "));
    }

    #[test]
    fn input_methods_commit_then_compose() {
        let events = Ime::from_protocol(Some("é".into()), ("ab".into(), 1, 2));
        assert_eq!(events, [Ime::Commit("é".into()), Ime::Preedit { text: "ab".into(), selection: Some(1..2) }]);
        assert_eq!(
            Ime::from_protocol(None, (String::new(), -1, -1)),
            [Ime::Preedit { text: String::new(), selection: None }]
        );
    }

    #[test]
    fn modifiers_combine() {
        let m = Modifiers::from_flags(modifier::SHIFT | modifier::OPTION | modifier::CAPS_LOCK);
        assert!(m.shift() && m.alt() && m.contains(Modifiers::CAPS_LOCK) && !m.control());
        assert_eq!(Modifiers::SHIFT | Modifiers::CONTROL, Modifiers::from_flags(modifier::SHIFT | modifier::CONTROL));
    }
}
