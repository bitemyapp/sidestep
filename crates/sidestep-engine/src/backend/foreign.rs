//! Windows kept over another program's window (xdg-foreign v2): that
//! program exports its window and passes the handle along (as an
//! xdg-desktop-portal request's `parent_window`, `wayland:<handle>`); we
//! import it and make our window its child, so the compositor keeps a
//! portal's dialog over the app that asked for it.

use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use smithay_client_toolkit::reexports::protocols::xdg::foreign::zv2::client::zxdg_imported_v2::{self, ZxdgImportedV2};
use smithay_client_toolkit::reexports::protocols::xdg::foreign::zv2::client::zxdg_importer_v2::{self, ZxdgImporterV2};

use super::State;

/// The importer, where the compositor offers one.
pub(super) struct Foreign {
    importer: Option<ZxdgImporterV2>,
}

impl Foreign {
    pub fn new(globals: &GlobalList, qh: &QueueHandle<State>) -> Foreign {
        Foreign { importer: globals.bind::<ZxdgImporterV2, _, _>(qh, 1..=1, ()).ok() }
    }

    /// Make `child` (a toplevel's surface) a child of the window `handle`
    /// names. The import is kept for as long as the relationship is: its
    /// destruction ends it. `None` without an importer; a handle the
    /// compositor doesn't know is told by its `destroyed` event.
    pub fn import(&self, handle: &str, child: &WlSurface, qh: &QueueHandle<State>) -> Option<ZxdgImportedV2> {
        let imported = self.importer.as_ref()?.import_toplevel(handle.to_owned(), qh, ());
        imported.set_parent_of(child);
        Some(imported)
    }
}

impl Dispatch<ZxdgImporterV2, ()> for State {
    fn event(
        _: &mut State,
        _: &ZxdgImporterV2,
        _: zxdg_importer_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZxdgImportedV2, ()> for State {
    fn event(
        _: &mut State,
        imported: &ZxdgImportedV2,
        event: zxdg_imported_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        // The parent went (or never was): the window stands on its own.
        if let zxdg_imported_v2::Event::Destroyed = event {
            imported.destroy();
        }
    }
}
