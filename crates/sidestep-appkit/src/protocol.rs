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

/// Height of a scroll layer's tiles, in device pixels.
pub(crate) const TILE_HEIGHT: u32 = 512;

/// The widest a scroll layer's tiles get, in device pixels: a layer
/// narrower than this has one column of tiles as wide as it is.
pub(crate) const TILE_WIDTH_MAX: u32 = 2048;

/// A tile, by its column and row: the tile holds the layer's device pixels
/// from `column × width` and `row × height` (a layer's points times its
/// scale), rows and columns counting from the layer's origin, so negative
/// ones lie above it or to its left.
pub(crate) type TileKey = [i32; 2];

/// Where a scroll layer's tiles are: their size in device pixels, and the
/// margin each keeps around it (so a tile placed at whole points always
/// has the pixels its crop starts at).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TileGrid {
    pub width: u32,
    pub height: u32,
    pub margin: u32,
}

impl TileGrid {
    /// The grid for a layer spanning `extent` (layer points) at `scale`:
    /// columns as wide as the layer, in steps of 64 pixels (so small
    /// changes of its width keep its tiles), up to [`TILE_WIDTH_MAX`].
    pub fn for_layer(extent: &Rect, scale: f64) -> TileGrid {
        let px = (((extent.x1 - extent.x0).max(1.0) as f64) * scale).ceil() as u32;
        TileGrid {
            width: px.div_ceil(64).saturating_mul(64).clamp(64, TILE_WIDTH_MAX),
            height: TILE_HEIGHT,
            margin: scale.ceil().max(1.0) as u32,
        }
    }

    /// A tile's pixels, margin included, on the layer's pixel grid: x0, y0,
    /// width, height.
    pub fn pixels(&self, key: TileKey) -> (i32, i32, u32, u32) {
        let m = self.margin as i32;
        (
            key[0] * self.width as i32 - m,
            key[1] * self.height as i32 - m,
            self.width + 2 * self.margin,
            self.height + 2 * self.margin,
        )
    }

    /// A tile's own rectangle, without its margin, in layer points.
    pub fn rect(&self, key: TileKey, scale: f64) -> Rect {
        let (w, h) = (self.width as f64, self.height as f64);
        let at = |v: f64| (v / scale) as f32;
        Rect::new(
            at(key[0] as f64 * w),
            at(key[1] as f64 * h),
            at((key[0] + 1) as f64 * w),
            at((key[1] + 1) as f64 * h),
        )
    }

    /// A tile's rectangle with its margin, in layer points: everything a
    /// paint must reach for the tile to hold it.
    pub fn padded(&self, key: TileKey, scale: f64) -> Rect {
        let (x, y, w, h) = self.pixels(key);
        let at = |v: f64| (v / scale) as f32;
        Rect::new(at(x as f64), at(y as f64), at((x as f64) + w as f64), at((y as f64) + h as f64))
    }

    /// The tiles whose own rectangles meet `r` (layer points): columns, then
    /// rows, as inclusive ranges.
    pub fn keys(&self, r: &Rect, scale: f64) -> Option<(std::ops::RangeInclusive<i32>, std::ops::RangeInclusive<i32>)> {
        if r.is_empty() {
            return None;
        }
        let span = |lo: f32, hi: f32, size: u32| {
            let size = size as f64;
            let first = ((lo as f64 * scale) / size).floor() as i32;
            // The last pixel the rectangle reaches, not the edge after it.
            let last = (((hi as f64 * scale) / size).ceil() as i32 - 1).max(first);
            first..=last
        };
        Some((span(r.x0, r.x1, self.width), span(r.y0, r.y1, self.height)))
    }

    /// The tiles whose rectangles with their margins meet `r`: the tiles a
    /// paint of `r` changes.
    pub fn padded_keys(
        &self,
        r: &Rect,
        scale: f64,
    ) -> Option<(std::ops::RangeInclusive<i32>, std::ops::RangeInclusive<i32>)> {
        let m = (self.margin as f64 / scale) as f32;
        self.keys(&Rect::new(r.x0 - m, r.y0 - m, r.x1 + m, r.y1 + m), scale)
    }
}

/// Where a scroll layer shows (see `layers`): all rectangles in whole
/// points of the window's content, top-left origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LayerPlace {
    /// Stacking among the window's layers, bottom first: the tiles', and
    /// the overlay's (above the layers nested in this one).
    pub z: [u32; 2],
    /// The part of the window the layer shows in: its clip view's visible
    /// part.
    pub viewport: Rect,
    /// Where the layer's point (0, 0) is in the window (on the device pixel
    /// grid).
    pub origin: [f32; 2],
    /// What the layer holds, in its points: its tiles cover this.
    pub extent: Rect,
    pub grid: TileGrid,
    /// The color an opaque layer's tiles are cleared to; `None` for a
    /// transparent layer, whose tiles show what's behind them.
    pub opaque: Option<Color>,
    /// The layer this one is nested in, [`ROOT_LAYER`] for none.
    pub parent: LayerId,
    /// Where the views drawn above the layer are, its overlay: in the
    /// points of the layer they are drawn in (`parent`), which places and
    /// clips it.
    pub overlay: Option<Rect>,
}

/// What a paint draws into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// The window's own surface; rectangles in window points.
    Root,
    /// A scroll layer's tiles; rectangles in the layer's points.
    Tiles(LayerId),
    /// A scroll layer's overlay; rectangles in window points.
    Overlay(LayerId),
}

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
    /// The user is resizing the window (a live resize).
    pub resizing: bool,
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
        /// Open as a sheet of this window: part of it, a subsurface placed
        /// top-centre under its title bar (see `backend::sheet`).
        sheet_of: Option<WindowId>,
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
    /// Repaint `rects` of a target from `ops`.
    Paint {
        window: WindowId,
        target: Target,
        rects: Vec<Rect>,
        ops: Vec<Op>,
    },
    /// Show a scroll layer, or show it somewhere else: sent when its
    /// placement changes. A new grid or opacity drops its tiles, a new
    /// overlay rectangle the overlay's pixels.
    PlaceLayer {
        window: WindowId,
        layer: LayerId,
        place: LayerPlace,
    },
    /// The layer is gone, with its tiles and overlay.
    DropLayer {
        window: WindowId,
        layer: LayerId,
    },
    /// Tiles the main thread no longer keeps drawn.
    DropTiles {
        window: WindowId,
        layer: LayerId,
        tiles: Vec<TileKey>,
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
        /// A press on a window that got the keyboard since its previous
        /// pointer event: the click that activated it (AppKit's first
        /// mouse).
        activating: bool,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_grids_follow_the_layer() {
        let grid = |w: f32, scale| TileGrid::for_layer(&Rect::new(0.0, 0.0, w, 100.0), scale);
        // As wide as the layer in 64-pixel steps, up to 2048.
        assert_eq!((grid(400.0, 1.0).width, grid(400.0, 2.0).width), (448, 832));
        assert_eq!((grid(3000.0, 1.0).width, grid(1100.0, 2.0).width), (2048, 2048));
        assert_eq!(grid(400.0, 1.0).height, 512);
        // A point of margin, in whole pixels.
        assert_eq!([1.0, 1.5, 2.0, 3.0].map(|s| grid(400.0, s).margin), [1, 2, 2, 3]);
    }

    #[test]
    fn tile_keys_count_from_the_origin() {
        let g = TileGrid { width: 2048, height: 512, margin: 1 };
        // Rows above the origin are negative; columns go on to the right.
        let keys = g.keys(&Rect::new(0.0, -400.0, 3000.0, 0.0), 1.0).expect("tiles");
        assert_eq!(keys, (0..=1, -1..=-1));
        let keys = g.keys(&Rect::new(10.0, 500.0, 20.0, 1030.0), 1.0).expect("tiles");
        assert_eq!(keys, (0..=0, 0..=2));
        // At scale 2 a row is 256 points.
        let keys = g.keys(&Rect::new(0.0, 250.0, 10.0, 260.0), 2.0).expect("tiles");
        assert_eq!(keys, (0..=0, 0..=1));
        // An edge on a boundary reaches no further.
        assert_eq!(g.keys(&Rect::new(0.0, 0.0, 10.0, 512.0), 1.0), Some((0..=0, 0..=0)));
        assert_eq!(g.keys(&Rect::default(), 1.0), None);
        // A paint reaching a margin reaches the tile.
        assert_eq!(g.padded_keys(&Rect::new(0.0, 512.5, 10.0, 520.0), 1.0), Some((-1..=0, 0..=1)));
        assert_eq!(g.rect([1, -1], 2.0), Rect::new(1024.0, -256.0, 2048.0, 0.0));
        assert_eq!(g.padded([0, 0], 1.0), Rect::new(-1.0, -1.0, 2049.0, 513.0));
    }
}
