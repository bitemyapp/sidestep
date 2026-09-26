//! What the main thread and the render thread tell each other.
//!
//! The main thread runs AppKit: events, timers, views. `drawRect:` doesn't
//! touch pixels; it records [`Op`]s for the parts of a layer that changed. The
//! render thread owns the Wayland connection, rasterizes those ops into its
//! layer caches and presents, and sends input and frame timing back.

/// A window, as the two threads name it.
pub(crate) type WindowId = u32;

/// A layer of a window: [`ROOT_LAYER`] for the window's own surface, or a
/// scroll view's document, named by its clip view.
pub(crate) type LayerId = u64;
pub(crate) const ROOT_LAYER: LayerId = 0;

/// Height of a scroll layer's tiles, in pixels.
pub(crate) const TILE_HEIGHT: u32 = 512;

/// A rectangle in a layer's pixel coordinates, top-left origin.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Rect { x0, y0, x1, y1 }
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        Rect { x0: self.x0.max(o.x0), y0: self.y0.max(o.y0), x1: self.x1.min(o.x1), y1: self.y1.min(o.y1) }
    }

    pub fn union(&self, o: &Rect) -> Rect {
        Rect { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }

    /// Grown to whole pixels.
    pub fn round_out(&self) -> Rect {
        Rect { x0: self.x0.floor(), y0: self.y0.floor(), x1: self.x1.ceil(), y1: self.y1.ceil() }
    }

    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect { x0: self.x0 + dx, y0: self.y0 + dy, x1: self.x1 + dx, y1: self.y1 + dy }
    }
}

/// Straight (not premultiplied) RGBA.
pub(crate) type Color = [f32; 4];

/// A recorded drawing operation, in layer coordinates, clipped to `clip`.
#[derive(Clone, Debug)]
pub(crate) enum Op {
    Fill { rect: Rect, color: Color },
    Path { points: Vec<[f32; 2]>, color: Color, clip: Rect },
    Text { x: f32, baseline: f32, size: f32, mono: bool, text: String, color: Color, clip: Rect },
}

/// How a window looks and what the user may do with it, from its style mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct Style {
    /// Has a title bar; without one (a borderless window) nothing is drawn
    /// around the content and the compositor is asked to draw nothing too.
    pub titled: bool,
    pub closable: bool,
    pub miniaturizable: bool,
    pub resizable: bool,
}

/// A window's size limits in points of content, `0` meaning none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct SizeLimits {
    pub min: (u32, u32),
    pub max: (u32, u32),
}

/// What the main thread asks of a window beyond drawing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WindowRequest {
    Minimize,
    Maximize(bool),
    Fullscreen(bool),
    /// Ask for keyboard focus (xdg-activation).
    Activate,
    /// A new content size in points.
    Resize(u32, u32),
}

/// The xdg_toplevel states the main thread cares about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct WindowState {
    pub maximized: bool,
    pub fullscreen: bool,
    pub activated: bool,
    pub tiled: bool,
}

/// Keyboard modifiers, as `NSEventModifierFlags` bits.
pub(crate) type Modifiers = usize;

/// A key press, repeat or release, already translated through the keymap.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Key {
    pub down: bool,
    pub repeat: bool,
    /// The XKB keycode (the evdev code plus 8).
    pub code: u16,
    pub characters: String,
    pub unmodified: String,
    pub modifiers: Modifiers,
}

/// A pointer button, by what it does rather than its Linux code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Button {
    Left,
    Right,
    /// Middle, back, forward and the rest, numbered as AppKit numbers
    /// buttons: 2 is the middle one.
    Other(u8),
}

/// A pointer's appearance, named as cursor themes and wp_cursor_shape_v1
/// name them.
pub(crate) use smithay_client_toolkit::seat::pointer::CursorIcon as Cursor;

/// What a popup is placed against: a rectangle in its parent window's
/// content (points, top-left origin). The popup opens below it (as menus
/// do), or with its top left corner at the rectangle's (as child windows
/// are placed).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PopupPlacement {
    pub parent: WindowId,
    pub anchor: Rect,
    pub below: bool,
    /// Take the keyboard and pointer grab, as menus do; tooltips don't.
    pub grab: bool,
}

pub(crate) enum ToRender {
    CreateWindow {
        window: WindowId,
        width: u32,
        height: u32,
        title: String,
        style: Style,
        limits: SizeLimits,
        /// Open as a popup (xdg_popup) instead of a toplevel.
        popup: Option<PopupPlacement>,
    },
    /// A new title, with its text set in the title bar's font as drawing
    /// ops (top-left origin, `width` by `height` points) for client-side
    /// decorations to show.
    SetTitle {
        window: WindowId,
        title: String,
        text: TitleText,
    },
    SetStyle {
        window: WindowId,
        style: Style,
    },
    SetSizeLimits {
        window: WindowId,
        limits: SizeLimits,
    },
    Request {
        window: WindowId,
        request: WindowRequest,
    },
    /// The pointer's appearance over the window's content.
    SetCursor {
        window: WindowId,
        cursor: Cursor,
    },
    /// Hide the pointer over every window's content, or show it again;
    /// `until_moved` shows it again at the pointer's next move.
    HideCursor {
        hidden: bool,
        until_moved: bool,
    },
    /// Repaint `rects` of a layer from `ops`.
    Paint {
        window: WindowId,
        layer: LayerId,
        rects: Vec<Rect>,
        ops: Vec<Op>,
    },
    /// Where a scroll layer shows: its viewport in window coordinates and
    /// the document position at the viewport's top edge.
    ScrollLayer {
        window: WindowId,
        layer: LayerId,
        viewport: Rect,
        offset: f32,
        doc_width: u32,
    },
    /// Tiles the main thread no longer keeps drawn.
    DropTiles {
        window: WindowId,
        layer: LayerId,
        tiles: Vec<u32>,
    },
    Present {
        window: WindowId,
    },
    CloseWindow {
        window: WindowId,
    },
    /// Offer these contents as the system selection (the clipboard), or
    /// clear it.
    SetSelection {
        contents: Option<std::sync::Arc<crate::clipboard::Contents>>,
    },
    /// Read the system selection as `mime`; the answer arrives in the
    /// clipboard's shared state under `token`.
    ReadSelection {
        mime: String,
        token: u64,
    },
}

/// A title set as drawing ops, `width` by `height` points.
#[derive(Clone, Debug, Default)]
pub(crate) struct TitleText {
    pub ops: Vec<Op>,
    pub width: f32,
    pub height: f32,
}

pub(crate) enum FromRender {
    /// The window's content size and scale, first and after every change,
    /// with its state and the height of the title bar drawn around it.
    Configure {
        window: WindowId,
        width: u32,
        height: u32,
        scale: f64,
        titlebar: u32,
        state: WindowState,
    },
    /// The last presented frame was shown; the next one may be sent.
    Frame {
        window: WindowId,
    },
    /// The window got or lost the keyboard.
    Focus {
        window: WindowId,
        focused: bool,
    },
    Key {
        window: WindowId,
        key: Key,
    },
    /// The modifier keys changed; `code` is the modifier key that changed
    /// them, if a key did.
    Modifiers {
        window: WindowId,
        modifiers: Modifiers,
        code: u16,
    },
    /// The pointer entered the window's content, at `x`, `y` points from
    /// its top left.
    Enter {
        window: WindowId,
        x: f64,
        y: f64,
    },
    Leave {
        window: WindowId,
    },
    Button {
        window: WindowId,
        x: f64,
        y: f64,
        button: Button,
        pressed: bool,
        /// 1 for a single click, 2 for a double click, and so on.
        clicks: u32,
        modifiers: Modifiers,
    },
    Motion {
        window: WindowId,
        x: f64,
        y: f64,
        modifiers: Modifiers,
    },
    /// Scrolling, positive toward the right and the bottom as Wayland
    /// counts: by `dx`, `dy` points from a touchpad or another continuous
    /// source, or by `dx`, `dy` detents (possibly fractional, from
    /// high-resolution wheels) when `wheel` is true.
    Scroll {
        window: WindowId,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        wheel: bool,
        modifiers: Modifiers,
    },
    CloseRequested {
        window: WindowId,
    },
    /// The compositor dismissed a popup.
    PopupDone {
        window: WindowId,
    },
}
