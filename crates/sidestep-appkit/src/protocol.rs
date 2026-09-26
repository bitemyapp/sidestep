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

/// Height of a scroll layer's tiles, in points (the render thread's tiles
/// are this many points times the scale in pixels).
pub(crate) const TILE_HEIGHT: u32 = 512;

/// A rectangle, top-left origin: in a layer's points in every message
/// (drawing ops, damage, viewports). The render thread also keeps rectangles
/// of device pixels in it, where a field says so.
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
}

/// Straight (not premultiplied) RGBA.
pub(crate) type Color = [f32; 4];

/// A recorded drawing operation, in layer coordinates.
///
/// `Fill` is the fast path: a rectangle already clipped, composited over
/// what's there. The others but glyphs carry a [`Draw`]: their transform
/// from user space, compositing, clip and shadow; the rasterizer
/// (`raster::ops`) draws them with tiny-skia.
#[derive(Clone, Debug)]
pub(crate) enum Op {
    Fill {
        rect: Rect,
        color: Color,
    },
    /// A clipped rectangle composited another way (`Copy` is how
    /// `NSRectFill` fills; `Clear` erases).
    FillWith {
        rect: Rect,
        color: Color,
        blend: Blend,
    },
    FillPath {
        path: Arc<tiny_skia::Path>,
        even_odd: bool,
        paint: Paint,
        draw: Draw,
    },
    StrokePath {
        path: Arc<tiny_skia::Path>,
        stroke: Arc<StrokeSpec>,
        paint: Paint,
        draw: Draw,
    },
    /// `src` of the image's pixels (top-left origin) drawn into `dst`
    /// (user space, `y0` its edge nearer the origin), faded by `alpha`; a
    /// template image is drawn in `tint` wherever it has alpha.
    Image {
        image: Arc<ImageData>,
        src: Rect,
        dst: Rect,
        alpha: f32,
        quality: Quality,
        tint: Option<Color>,
        draw: Draw,
    },
    /// What follows until the matching `EndGroup` is drawn into a
    /// transparent layer, then composited with `alpha` and `draw.blend`
    /// (`NSView.alphaValue`). `draw.xf` is unused.
    BeginGroup {
        alpha: f32,
        draw: Draw,
    },
    EndGroup,
    Glyphs(GlyphRun),
}

pub(crate) use crate::raster::images::ImageData;
use std::sync::Arc;

/// How an op combines with what it's drawn over: `NSCompositingOperation`,
/// value for value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub(crate) enum Blend {
    Clear,
    Copy,
    #[default]
    SourceOver,
    SourceIn,
    SourceOut,
    SourceAtop,
    DestinationOver,
    DestinationIn,
    DestinationOut,
    DestinationAtop,
    Xor,
    PlusDarker,
    Highlight,
    PlusLighter,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl Blend {
    /// The `NSCompositingOperation` value `op`; out-of-range ones are
    /// source over.
    pub fn from_raw(op: usize) -> Blend {
        const ALL: [Blend; 29] = [
            Blend::Clear,
            Blend::Copy,
            Blend::SourceOver,
            Blend::SourceIn,
            Blend::SourceOut,
            Blend::SourceAtop,
            Blend::DestinationOver,
            Blend::DestinationIn,
            Blend::DestinationOut,
            Blend::DestinationAtop,
            Blend::Xor,
            Blend::PlusDarker,
            Blend::Highlight,
            Blend::PlusLighter,
            Blend::Multiply,
            Blend::Screen,
            Blend::Overlay,
            Blend::Darken,
            Blend::Lighten,
            Blend::ColorDodge,
            Blend::ColorBurn,
            Blend::SoftLight,
            Blend::HardLight,
            Blend::Difference,
            Blend::Exclusion,
            Blend::Hue,
            Blend::Saturation,
            Blend::Color,
            Blend::Luminosity,
        ];
        ALL.get(op).copied().unwrap_or_default()
    }
}

/// What an op needs besides its shape and paint.
#[derive(Clone, Debug)]
pub(crate) struct Draw {
    /// User space to layer points.
    pub xf: tiny_skia::Transform,
    pub blend: Blend,
    pub aa: bool,
    /// Where the op may draw, in layer points: the clip's bounds.
    pub clip: Rect,
    /// The clip's shape where it isn't `clip` itself: paths the op must be
    /// inside of, all of them.
    pub mask: Option<Arc<[ClipPath]>>,
    pub shadow: Option<Arc<ShadowSpec>>,
}

/// A path clipping ops: layer points are `xf` of its points.
#[derive(Clone, Debug)]
pub(crate) struct ClipPath {
    pub path: Arc<tiny_skia::Path>,
    pub even_odd: bool,
    pub xf: tiny_skia::Transform,
    pub aa: bool,
}

/// What a shape is filled or stroked with.
#[derive(Clone, Debug)]
pub(crate) enum Paint {
    Solid(Color),
    Gradient(Arc<GradientSpec>),
}

/// A linear gradient from `start` to `end`, or a radial one between two
/// circles, in the op's user space. `extend` says whether the end colors
/// continue before the start and after the end.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GradientSpec {
    pub stops: Vec<(f32, Color)>,
    pub start: (f32, f32),
    pub end: (f32, f32),
    /// The circles' radii, for a radial gradient.
    pub radii: Option<(f32, f32)>,
    pub extend: (bool, bool),
}

/// How a path is stroked, in user space: `NSLineCapStyle` and
/// `NSLineJoinStyle` values, and a dash pattern with its phase.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StrokeSpec {
    /// 0 draws the thinnest line the device can show.
    pub width: f32,
    pub cap: u8,
    pub join: u8,
    pub miter: f32,
    pub dash: Option<(Vec<f32>, f32)>,
}

/// A shadow: offset in layer points (y down), blur radius and color.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShadowSpec {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub color: Color,
}

/// How images are resampled (`NSImageInterpolation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Quality {
    None,
    Low,
    #[default]
    Medium,
    High,
}

/// Text, shaped and laid out on the main thread: glyphs of one face in one
/// color. Lines drawn again share their glyphs through the `Arc`.
#[derive(Clone, Debug)]
pub(crate) struct GlyphRun {
    /// The face, as `text::fonts` registered it.
    pub font: u32,
    /// Font size in points.
    pub size: f32,
    /// The run's origin on its baseline, in layer coordinates.
    pub x: f32,
    pub y: f32,
    pub glyphs: std::sync::Arc<[Glyph]>,
    pub color: Color,
    pub clip: Rect,
}

/// A glyph of a [`GlyphRun`]: its id in the face and its position relative
/// to the run's origin, in points, y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Glyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
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
    /// Dragging the title bar moves the window.
    pub movable: bool,
    /// Pointer input goes through the window to whatever is behind it.
    pub passthrough: bool,
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
    /// Move the window with the pointer, from the button press that's
    /// down.
    Move,
}

/// The xdg_toplevel states the main thread cares about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct WindowState {
    pub maximized: bool,
    pub fullscreen: bool,
    pub activated: bool,
    pub tiled: bool,
    /// The compositor isn't showing the window (hidden behind others,
    /// minimized, on another workspace).
    pub suspended: bool,
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
    /// A compose sequence as it stands after the key, when it started,
    /// continued or ended one (empty once it's over).
    pub composing: Option<String>,
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
    /// Read the system selection (or drag `drag`'s offer) as `mime`; the
    /// answer arrives in the clipboard's shared state under `token`.
    ReadSelection {
        mime: String,
        token: u64,
        source: crate::clipboard::Source,
        drag: u64,
    },
    /// The answer to `FromRender::ProvideSelection` `token`: the promised
    /// data, or nothing.
    SelectionData {
        token: u64,
        data: Option<std::sync::Arc<[u8]>>,
    },
    /// The answer to drag `drag`'s latest position, tick or source
    /// actions: the MIME type the destination takes (none: it refuses), the
    /// Wayland actions it accepts and prefers, and whether it wants
    /// periodic updates (`FromRender::DndTick`).
    DndStatus {
        drag: u64,
        mime: Option<String>,
        actions: u32,
        preferred: u32,
        periodic: bool,
    },
    /// Drag `drag`'s drop is over; `performed` if the destination took the
    /// data.
    DndFinish {
        drag: u64,
        performed: bool,
    },
    /// Publish the outputs for `NSScreen` (sent first, to start the render
    /// thread, when a program asks for screens before showing a window).
    PublishOutputs,
    /// Whether the window's first responder takes text from input methods,
    /// and where its caret is, in points from the top left of the content.
    TextInput {
        window: WindowId,
        wanted: bool,
        caret: Option<Rect>,
    },
    /// The program dropped the text being composed: the input method starts
    /// over.
    ResetTextInput {
        window: WindowId,
    },
    /// The desktop's light or dark preference changed (from the settings
    /// portal's watcher).
    ColorScheme {
        dark: bool,
    },
    /// The window belongs over `parent` (a titled child window, or a modal
    /// one), or over none.
    SetParent {
        window: WindowId,
        parent: Option<WindowId>,
    },
    /// Image representations that went away: drop what's cached of them.
    ForgetImages {
        keys: Vec<u64>,
    },
}

/// Where a scroll is in a touchpad gesture; `None` for wheels and other
/// sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollPhase {
    None,
    Began,
    Changed,
    Ended,
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
    ///
    /// Touchpad scrolls come in gestures: `Began`, `Changed`s, then `Ended`
    /// (no distance) with the fingers' speed at the end, in points per
    /// second, for scrolling to coast on. `inverted`: the compositor
    /// reverses the device's direction (natural scrolling).
    Scroll {
        window: WindowId,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        wheel: bool,
        modifiers: Modifiers,
        phase: ScrollPhase,
        velocity: (f64, f64),
        inverted: bool,
    },
    /// A two-finger pinch on a touchpad: the change of scale since the last
    /// update as a factor less one, and the rotation, in degrees
    /// counterclockwise.
    Pinch {
        window: WindowId,
        x: f64,
        y: f64,
        phase: ScrollPhase,
        magnification: f64,
        rotation: f64,
        modifiers: Modifiers,
    },
    CloseRequested {
        window: WindowId,
    },
    /// The compositor dismissed a popup.
    PopupDone {
        window: WindowId,
    },
    /// An input method's changes: text to insert, then the text being
    /// composed (empty when none) with its selection, in bytes (-1 for no
    /// caret).
    TextInput {
        window: WindowId,
        commit: Option<String>,
        preedit: (String, i32, i32),
    },
    /// Another client asked for a type of our selection that the program
    /// promised: answer `ToRender::SelectionData` under `token`.
    ProvideSelection {
        mime: String,
        token: u64,
    },
    /// Drag `drag` (the render thread's name for it, which the answers
    /// carry) entered the window's content at `x`, `y` (points from its top
    /// left), offering `mimes`, the source allowing Wayland `actions`, with
    /// its URL list (`urls`: the MIME type and data) if it offers one. This,
    /// `DndMotion`, `DndActions` and `DndTick` each get one
    /// `ToRender::DndStatus`.
    DndEnter {
        drag: u64,
        window: WindowId,
        x: f64,
        y: f64,
        mimes: Vec<String>,
        actions: u32,
        urls: Option<(String, std::sync::Arc<[u8]>)>,
    },
    DndMotion {
        drag: u64,
        x: f64,
        y: f64,
    },
    /// The source's allowed actions changed.
    DndActions {
        drag: u64,
        actions: u32,
    },
    /// The drag is still there, wherever it last was: a periodic update,
    /// answered like a move.
    DndTick {
        drag: u64,
    },
    DndLeave {
        drag: u64,
    },
    /// Dropped where the drag last was; answered with `ToRender::DndFinish`.
    DndDrop {
        drag: u64,
    },
    /// The outputs changed; the new ones are published for `NSScreen`.
    ScreensChanged,
    /// The outputs the window's surface is on, in the order it entered
    /// them.
    WindowOutputs {
        window: WindowId,
        outputs: Vec<u32>,
    },
    /// The desktop's appearance changed (from the settings thread, which
    /// sends this only to wake the main thread; `settings` has the rest).
    Appearance,
}
