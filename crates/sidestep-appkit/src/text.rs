//! Text: fonts, layout and glyph rasterization.
//!
//! AppKit measures text synchronously (`sizeWithAttributes:`,
//! `boundingRectWithSize:options:attributes:context:`), so text is shaped
//! and laid out on the thread that asks, usually the main one, and the
//! result is cached by string and attributes. Drawing then records
//! [`GlyphRun`](crate::protocol::GlyphRun)s: a registered face, a size,
//! glyph ids and positions in points; text drawn before it was ever
//! measured is laid out at the end of the display pass, on several threads
//! at once. The render thread never sees a string; it rasterizes each
//! glyph once per size and subpixel offset into a cache and composites
//! from there.
//!
//! - [`fonts`]: the system's families through fontconfig, faces for
//!   `NSFont`, and the registry that names faces across threads.
//! - [`layout`]: attribute runs and paragraph styles into lines, with
//!   parley doing bidi, line breaking and shaping.
//! - [`lines`]: the same lines with their UTF-16 ranges, clusters and
//!   carets, a paragraph or a few lines at a time, for TextKit.
//! - [`glyphs`]: the same lines as CoreText describes them, every glyph
//!   with its position, advance and character index, for `CTLine`.
//! - [`pool`]: worker threads for laying many lines out at once.
//! - [`raster`]: the render thread's glyph cache and compositing.
//!
//! Each thread that lays text out keeps a [`Ctx`]: a parley font context
//! cloned from the shared one (clones share the system's fonts and loaded
//! font files), parley's scratch space, and its caches.

#[cfg(test)]
mod bench;
pub(crate) mod fonts;
pub(crate) mod glyphs;
pub(crate) mod layout;
// TextKit (NSLayoutManager, NSTextView), which a later workstream builds,
// is what lays text out through this; until then only its tests do.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod lines;
mod pool;
mod raster;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) use raster::{Glyphs, draw_glyphs};

/// What parley carries per style: the index of the run's attributes, which
/// hold everything that doesn't change shaping (colors, decorations, the
/// baseline offset).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Brush(pub u32);

/// A thread's text state.
pub(crate) struct Ctx {
    pub fcx: parley::FontContext,
    pub lcx: parley::LayoutContext<Brush>,
    faces: HashMap<fonts::FaceKey, Arc<fonts::Face>>,
    pub layouts: layout::Cache,
    /// A parley layout lent to each new layout, to reuse its allocations.
    scratch: parley::Layout<Brush>,
}

thread_local!(static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) });

/// Run `f` with this thread's text state. `f` must not call back into
/// Objective-C code that could draw or measure text.
pub(crate) fn with_ctx<R>(f: impl FnOnce(&mut Ctx) -> R) -> R {
    CTX.with(|cell| {
        let mut slot = cell.borrow_mut();
        let ctx = slot.get_or_insert_with(|| Ctx {
            fcx: fonts::shared().font_context(),
            lcx: parley::LayoutContext::new(),
            faces: HashMap::new(),
            layouts: layout::Cache::default(),
            scratch: parley::Layout::new(),
        });
        f(ctx)
    })
}
