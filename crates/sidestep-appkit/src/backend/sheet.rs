//! Sheets on screen. A sheet is part of its parent window, as on macOS: its
//! root surface is a desynchronized subsurface of the parent's root surface,
//! placed top-centre under the parent's title bar (the top of its content)
//! and moved when the parent resizes. It has its own canvas, buffers and
//! frame callbacks, so it draws and presents on its own; input over it goes
//! to the sheet's window by its surface's role, and the keyboard, which the
//! compositor gives the parent's toplevel, the main thread hands to the
//! sheet.
//!
//! The stacking among a window's subsurfaces is kept here in one place: a
//! sheet is above its parent's scroll tiles, including tiles made after it.
//! (Decorations sit outside the content, where sheets don't reach.)

use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::protocol::wl_subsurface::WlSubsurface;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;

use super::{Shell, State};
use crate::protocol::WindowId;

/// A sheet's shell: its subsurface of the parent, and its own surface.
pub(crate) struct Sheet {
    pub(crate) parent: WindowId,
    subsurface: WlSubsurface,
    surface: WlSurface,
}

impl Sheet {
    pub(crate) fn surface(&self) -> &WlSurface {
        &self.surface
    }
}

impl Drop for Sheet {
    fn drop(&mut self) {
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

/// A new sheet's shell over `parent`, which must be on screen.
pub(crate) fn create(state: &State, parent: WindowId) -> Option<Sheet> {
    let parent_surface = state.windows.get(&parent)?.surface().clone();
    let (subsurface, surface) = state.subcompositor.create_subsurface(parent_surface, &state.qh);
    // Drawn and presented on its own, not with the parent's commits.
    subsurface.set_desync();
    Some(Sheet { parent, subsurface, surface })
}

/// Put `window`'s sheet top-centre in its parent. The position applies with
/// the parent's next commit, which this makes.
pub(crate) fn place(state: &State, window: WindowId) {
    let Some(win) = state.windows.get(&window) else { return };
    let Shell::Sheet(sheet) = &win.shell else { return };
    let Some(parent) = state.windows.get(&sheet.parent) else { return };
    let x = (parent.width as i32 - win.width as i32) / 2;
    sheet.subsurface.set_position(x.max(0), 0);
    parent.surface().commit();
}

/// The sheets on screen over `parent`.
fn sheets_of(state: &State, parent: WindowId) -> impl Iterator<Item = (WindowId, &Sheet)> + '_ {
    state.windows.iter().filter_map(move |(&id, win)| match &win.shell {
        Shell::Sheet(sheet) if sheet.parent == parent => Some((id, sheet)),
        _ => None,
    })
}

/// `parent` resized: its sheets stay centred.
pub(crate) fn parent_resized(state: &State, parent: WindowId) {
    let sheets: Vec<WindowId> = sheets_of(state, parent).map(|(id, _)| id).collect();
    for sheet in sheets {
        place(state, sheet);
    }
}

/// The surfaces of the sheets over `parent`, for a new tile to go below.
pub(crate) fn surfaces_over(state: &State, parent: WindowId) -> Vec<WlSurface> {
    sheets_of(state, parent).map(|(_, sheet)| sheet.surface.clone()).collect()
}

/// Keep a subsurface of the parent's (a new tile) below its sheets.
pub(crate) fn keep_below(subsurface: &WlSubsurface, sheets: &[WlSurface]) {
    for sheet in sheets.iter().filter(|s| s.is_alive()) {
        subsurface.place_below(sheet);
    }
}

/// The toplevel a window belongs to: itself, or a sheet's parent.
pub(crate) fn toplevel_of(state: &State, window: WindowId) -> WindowId {
    match state.windows.get(&window).map(|w| &w.shell) {
        Some(Shell::Sheet(sheet)) => sheet.parent,
        _ => window,
    }
}
