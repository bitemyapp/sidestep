//! The main menu's bar in a window, on the render thread (the main thread's
//! part is `crate::menubar`): a subsurface of the toplevel's frame
//! (`Frame`) just above the content, under the header when Sidestep draws
//! one, and inside the window geometry. The window grows by it and its
//! content keeps its size; to the main thread the bar is part of the title
//! bar (`Win::titlebar`), so frames, popups, sheets and size limits take it
//! in as they take the header.
//!
//! The main thread draws the bar (its ground and the menus' titles) as
//! ops. They're rasterized here at the window's scale, again only when
//! they, the width or the scale change, into a canvas the bar keeps, then
//! copied into a buffer as wide as the window in the opaque format the
//! window's own buffers use (the bar is opaque). Like the decorations the
//! bar is a synchronized subsurface (`decor::Surface`), shown with the
//! window's next commit, and it hides in full screen. Pointer
//! input on it is the content's, above its top edge (`Role::Bar`), so the
//! main thread sees a click on the bar as a click in the window above its
//! content.

use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::shm::slot::SlotPool;

use super::decor::Surface;
use super::{Role, State, copy_pixels, format, px};
use crate::protocol::{Op, Rect, WindowId};
use crate::raster::{self, Canvas, Glyphs};

pub(crate) struct Bar {
    surface: Surface,
    /// In points.
    pub height: u32,
    ops: Vec<Op>,
    /// What it is painted into, kept for the next time (the same size is
    /// painted over; a new one reallocates).
    canvas: Vec<u32>,
    /// The width, height and scale it was last drawn at; none to draw it
    /// again.
    drawn: Option<(u32, u32, u64)>,
}

impl Bar {
    /// Whether it has something new to show.
    pub fn needs_draw(&self) -> bool {
        self.drawn.is_none()
    }

    /// Draw it for a window `width` points wide at `scale`, `height` points
    /// tall (0 hides it), and place it above the content, whose top is `top`
    /// points down the window's main surface; `rgba` as the window's buffers
    /// take pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        width: u32,
        height: u32,
        top: i32,
        scale: f64,
        rgba: bool,
        pool: &mut SlotPool,
        glyphs: &mut Glyphs,
    ) {
        if height == 0 {
            self.surface.hide();
            self.drawn = None;
            return;
        }
        let at = (0, top - height as i32);
        let key = (width, height, scale.to_bits());
        if self.drawn == Some(key) && self.surface.mapped {
            self.surface.place(at);
            return;
        }
        let (pw, ph) = (px(width, scale).max(1), px(height, scale).max(1));
        self.canvas.clear();
        self.canvas.resize((pw * ph) as usize, 0);
        let mut canvas = Canvas::new(&mut self.canvas, pw, ph, 0.0, scale as f32);
        raster::paint(&mut canvas, glyphs, &[Rect::new(0.0, 0.0, width as f32, height as f32)], &self.ops);
        let pixels = &self.canvas;
        self.surface.show_as(pool, (pw, ph), format(rgba), at, (width, height), |dst| {
            copy_pixels(dst, pixels, rgba);
        });
        self.drawn = Some(key);
    }
}

/// `ToRender::MenuBar`: give the window a bar `height` points tall showing
/// `ops`, or take its bar away for 0. Only toplevels have one.
pub(super) fn set(state: &mut State, window: WindowId, height: u32, ops: Vec<Op>) {
    let Some(win) = state.windows.get_mut(&window) else { return };
    if win.toplevel().is_none() {
        return;
    }
    let before = win.titlebar();
    if height == 0 {
        if let Some(bar) = win.menubar.take() {
            state.roles.remove(&bar.surface.surface.id());
        }
    } else if let Some(bar) = &mut win.menubar {
        bar.height = height;
        bar.ops = ops;
        bar.drawn = None;
    } else {
        let viewporter = state.viewporter.get().expect("viewporter");
        let surface = Surface::new(win.main_surface(), &state.subcompositor, viewporter, &state.qh);
        if win.passthrough {
            surface.surface.set_input_region(state.empty_region.as_ref().map(|r| r.wl_region()));
        }
        state.roles.insert(surface.surface.id(), Role::Bar(window));
        win.menubar = Some(Bar { surface, height, ops, canvas: Vec::new(), drawn: None });
    }
    let (after, configured) = (win.titlebar(), win.configured);
    if before != after {
        state.apply_limits(window);
    }
    if configured {
        // Shown with the window's next commit, now; the main thread hears
        // of the taller title bar.
        state.refresh_decor(window);
        state.report(window);
    }
}
