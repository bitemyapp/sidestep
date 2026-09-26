//! The clipboard on the render thread: a wl_data_device per seat.
//!
//! Offering: the main thread's contents become a wl_data_source offering
//! each MIME type plus a marker type naming this process, set as the
//! selection with the serial of the latest input event. Other clients'
//! requests are written from the event loop a pipe-buffer at a time, so a
//! slow reader never blocks this thread.
//!
//! Receiving: when another client's selection arrives, its text is read
//! ahead through a pipe serviced by the event loop, and both the types and
//! the text land in the shared state `NSPasteboard` reads (see
//! [`crate::clipboard`]). Selections carrying our marker are our own and
//! aren't read back.

use std::io::{Read, Write};
use std::sync::Arc;

use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceData, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferData, DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::{CopyPasteSource, DataSourceData, DataSourceHandler};
use smithay_client_toolkit::data_device_manager::{DataDeviceManagerState, ReadPipe, WritePipe};
use smithay_client_toolkit::globals::GlobalData;
use smithay_client_toolkit::reexports::calloop::PostAction;
use smithay_client_toolkit::reexports::client::backend::ObjectData;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device::{self, WlDataDevice};
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use smithay_client_toolkit::reexports::client::protocol::wl_data_offer::WlDataOffer;
use smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, delegate_dispatch};

use super::State;
use crate::clipboard::{Contents, mime_types, owner_mime, shared};

/// The most a selection read keeps; more is cut off.
const READ_LIMIT: usize = 256 << 20;
/// Written per wakeup: a pipe that polls writable takes this much without
/// blocking.
const WRITE_CHUNK: usize = 4096;

pub(crate) struct Selection {
    manager: Option<DataDeviceManagerState>,
    devices: Vec<(WlSeat, DataDevice)>,
    /// Our offer and what it holds.
    source: Option<(CopyPasteSource, Arc<Contents>)>,
    /// Contents to offer once there is an input serial to offer them with.
    pending: Option<Option<Arc<Contents>>>,
    /// We cleared the selection; the compositor's word of it is no news.
    cleared_by_us: bool,
}

impl Selection {
    pub fn new(globals: &GlobalList, qh: &QueueHandle<State>) -> Self {
        Selection {
            manager: DataDeviceManagerState::bind(globals, qh).ok(),
            devices: Vec::new(),
            source: None,
            pending: None,
            cleared_by_us: false,
        }
    }
}

pub(super) fn add_seat(state: &mut State, seat: &WlSeat) {
    if let Some(manager) = &state.selection.manager {
        let device = manager.get_data_device(&state.qh, seat);
        state.selection.devices.push((seat.clone(), device));
    }
}

pub(super) fn remove_seat(state: &mut State, seat: &WlSeat) {
    state.selection.devices.retain(|(s, _)| s != seat);
}

/// A window got the keyboard: contents written before there was an input
/// serial can be offered now.
pub(super) fn focus_changed(state: &mut State) {
    if let Some(contents) = state.selection.pending.take() {
        set(state, contents);
    }
}

/// Offer `contents` as the selection, or clear it.
pub(super) fn set(state: &mut State, contents: Option<Arc<Contents>>) {
    let Some(manager) = &state.selection.manager else { return };
    let latest = state.seats.latest_serial();
    let device = latest.as_ref().and_then(|(seat, _)| state.selection.devices.iter().find(|(s, _)| s == seat));
    let (Some((_, device)), Some((_, serial))) = (device, latest) else {
        state.selection.pending = Some(contents);
        return;
    };
    match contents {
        Some(contents) => {
            let mimes = contents.items.iter().map(|(m, _)| m.clone()).chain([owner_mime().to_owned()]);
            let source = manager.create_copy_paste_source(&state.qh, mimes);
            source.set_selection(device, serial);
            state.selection.source = Some((source, contents));
        }
        None => {
            device.unset_selection(serial);
            state.selection.source = None;
            state.selection.cleared_by_us = true;
        }
    }
}

/// Read the selection as `mime` for the main thread, answering under
/// `token`.
pub(super) fn read(state: &mut State, mime: String, token: u64) {
    let offer = state
        .selection
        .devices
        .iter()
        .find_map(|(_, d)| d.data().selection_offer())
        .filter(|o| o.with_mime_types(|m| m.contains(&mime)));
    let pipe = offer.and_then(|o| o.receive(mime).ok());
    match pipe {
        Some(pipe) => read_pipe(state, pipe, move |data| shared().read_arrived(token, data)),
        None => shared().read_arrived(token, None),
    }
}

/// Read a pipe to its end from the event loop, then hand over the bytes
/// (or nothing, on an error).
fn read_pipe(state: &State, pipe: ReadPipe, done: impl FnOnce(Option<Vec<u8>>) + 'static) {
    let mut data = Vec::new();
    let mut done = Some(done);
    let mut finish = move |result: Option<Vec<u8>>| {
        if let Some(done) = done.take() {
            done(result);
        }
        PostAction::Remove
    };
    // If the source can't be added, the reader times out instead.
    let _ = state.loop_handle.insert_source(pipe, move |(), file, _| {
        let mut chunk = [0u8; 16384];
        // One read per wakeup: the pipe is blocking, but readable now.
        match (&**file).read(&mut chunk) {
            Ok(0) => finish(Some(std::mem::take(&mut data))),
            Ok(n) if data.len() + n > READ_LIMIT => finish(Some(std::mem::take(&mut data))),
            Ok(n) => {
                data.extend_from_slice(&chunk[..n]);
                PostAction::Continue
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => PostAction::Continue,
            Err(_) => finish(None),
        }
    });
}

/// Text, as the best text type an offer has.
fn text_mime(mimes: &[String]) -> Option<String> {
    mime_types("public.utf8-plain-text").into_iter().find(|m| mimes.contains(m))
}

impl DataDeviceHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, _: &WlSurface) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        let Some(offer) = device.data::<DataDeviceData>().and_then(|d| d.selection_offer()) else { return };
        let mimes = offer.with_mime_types(|m| m.to_vec());
        if mimes.iter().any(|m| m == owner_mime()) {
            // Our own offer, which the main thread already holds.
            return;
        }
        let text = text_mime(&mimes);
        let generation = shared().foreign_changed(mimes, text.is_some());
        let Some(mime) = text else { return };
        match offer.receive(mime) {
            Ok(pipe) => read_pipe(self, pipe, move |data| {
                let text = data.map(|d| String::from_utf8_lossy(&d).into_owned());
                shared().text_arrived(generation, text);
            }),
            Err(_) => shared().text_arrived(generation, None),
        }
    }
}

impl DataOfferHandler for State {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for State {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        fd: WritePipe,
    ) {
        let data = self.selection.source.as_ref().filter(|(s, _)| s.inner() == source).and_then(|(_, c)| c.data(&mime));
        // Without data the pipe closes here, and the reader gets nothing.
        let Some(data) = data else { return };
        let mut written = 0;
        let _ = self.loop_handle.insert_source(fd, move |(), file, _| {
            let end = (written + WRITE_CHUNK).min(data.len());
            match (&**file).write(&data[written..end]) {
                Ok(n) => {
                    written += n;
                    if written >= data.len() { PostAction::Remove } else { PostAction::Continue }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => PostAction::Continue,
                // The reader went away.
                Err(_) => PostAction::Remove,
            }
        });
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.selection.source.as_ref().is_some_and(|(s, _)| s.inner() == source) {
            self.selection.source = None;
        }
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

/// The data device's events go through smithay-client-toolkit, which
/// doesn't report a cleared selection; this notices those first.
impl Dispatch<WlDataDevice, DataDeviceData> for State {
    fn event(
        state: &mut State,
        device: &WlDataDevice,
        event: wl_data_device::Event,
        data: &DataDeviceData,
        conn: &Connection,
        qh: &QueueHandle<State>,
    ) {
        let cleared = matches!(event, wl_data_device::Event::Selection { id: None });
        <DataDeviceManagerState as Dispatch<WlDataDevice, DataDeviceData, State>>::event(
            state, device, event, data, conn, qh,
        );
        if cleared && !std::mem::take(&mut state.selection.cleared_by_us) {
            shared().foreign_changed(Vec::new(), false);
        }
    }

    fn event_created_child(opcode: u16, qh: &QueueHandle<State>) -> Arc<dyn ObjectData> {
        <DataDeviceManagerState as Dispatch<WlDataDevice, DataDeviceData, State>>::event_created_child(opcode, qh)
    }
}

delegate_dispatch!(State: [WlDataDeviceManager: GlobalData] => DataDeviceManagerState);
delegate_dispatch!(State: [WlDataOffer: DataOfferData] => DataDeviceManagerState);
delegate_dispatch!(State: [WlDataSource: DataSourceData] => DataDeviceManagerState);
