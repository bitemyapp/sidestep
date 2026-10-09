//! Sheets on screen. A sheet is part of its parent window, as on macOS: its
//! root surface is a desynchronized subsurface of the parent's main surface
//! (a toplevel's frame), placed top-centre under the parent's title bar (the
//! top of its content) and moved when the parent resizes or its title bar
//! changes. It has its own canvas, buffers and frame callbacks, so it draws
//! and presents on its own; input over it goes to the sheet's window by its
//! surface's role, and the keyboard, which the compositor gives the
//! parent's toplevel, the main thread hands to the sheet.
//!
//! A subsurface's position belongs to the parent's state, applied by the
//! parent's next commit. When the parent resizes, its own present at the
//! new size (which always follows) applies it, so a configure never makes
//! an extra commit; when the sheet is made or resized, the parent is
//! committed for it at once, unless the parent has a new size it hasn't
//! drawn yet (committing its old buffer then would break the compositor's
//! rules for maximized and full-screen windows) and its present is coming.
//!
//! A sheet is above its parent's scroll tiles, including tiles made after
//! it: `tiles` stacks every layer's surfaces directly above the content's
//! surface, so below its sheets. (Decorations sit outside the content,
//! where sheets don't reach.)

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
    let parent_surface = state.windows.get(&parent)?.main_surface().clone();
    let (subsurface, surface) = state.subcompositor.create_subsurface(parent_surface, &state.qh);
    // Drawn and presented on its own, not with the parent's commits.
    subsurface.set_desync();
    Some(Sheet { parent, subsurface, surface })
}

/// Put `window`'s sheet top-centre in its parent, as of the parent's next
/// commit.
pub(crate) fn place(state: &State, window: WindowId) {
    let Some(win) = state.windows.get(&window) else { return };
    let Shell::Sheet(sheet) = &win.shell else { return };
    let Some(parent) = state.windows.get(&sheet.parent) else { return };
    let x = (parent.width as i32 - win.width as i32) / 2;
    sheet.subsurface.set_position(x.max(0), parent.titlebar() as i32);
}

/// Show where a sheet made or resized was placed, by committing its parent,
/// if the parent shows its current size (see the module's notes).
pub(crate) fn commit_parent(state: &State, window: WindowId) {
    let Some(Shell::Sheet(sheet)) = state.windows.get(&window).map(|w| &w.shell) else { return };
    let Some(parent) = state.windows.get(&sheet.parent) else { return };
    if parent.geometry == parent.expected_geometry() {
        parent.main_surface().commit();
    }
}

/// The sheets on screen over `parent`.
fn sheets_of(state: &State, parent: WindowId) -> impl Iterator<Item = (WindowId, &Sheet)> + '_ {
    state.windows.iter().filter_map(move |(&id, win)| match &win.shell {
        Shell::Sheet(sheet) if sheet.parent == parent => Some((id, sheet)),
        _ => None,
    })
}

/// `parent` took a new size or title bar: its sheets stay centred under the
/// title bar, as of its present for them.
pub(crate) fn parent_resized(state: &State, parent: WindowId) {
    let sheets: Vec<WindowId> = sheets_of(state, parent).map(|(id, _)| id).collect();
    for sheet in sheets {
        place(state, sheet);
    }
}

/// The toplevel a window belongs to: itself, or a sheet's parent.
pub(crate) fn toplevel_of(state: &State, window: WindowId) -> WindowId {
    match state.windows.get(&window).map(|w| &w.shell) {
        Some(Shell::Sheet(sheet)) => sheet.parent,
        _ => window,
    }
}
