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
    Glyphs(GlyphRun),
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

pub(crate) enum ToRender {
    CreateWindow {
        window: WindowId,
        width: u32,
        height: u32,
        title: String,
    },
    SetTitle {
        window: WindowId,
        title: String,
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
}

pub(crate) enum FromRender {
    /// The window's size, first and after every resize.
    Configure {
        window: WindowId,
        width: u32,
        height: u32,
    },
    /// The last presented frame was shown; the next one may be sent.
    Frame {
        window: WindowId,
    },
    Button {
        window: WindowId,
        x: f64,
        y: f64,
        button: u32,
        pressed: bool,
    },
    Motion {
        window: WindowId,
        x: f64,
        y: f64,
    },
    Scroll {
        window: WindowId,
        x: f64,
        y: f64,
        dy: f64,
    },
    CloseRequested {
        window: WindowId,
    },
}
