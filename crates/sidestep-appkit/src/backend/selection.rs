//! The clipboard on the render thread: a wl_data_device per seat.
//!
//! Offering: the main thread's contents become a wl_data_source offering
//! each MIME type plus a marker type naming this process, set as the
//! selection with the serial of the latest input event. Other clients'
//! requests are written from the event loop through non-blocking pipes, as
//! much as each takes per wakeup, so a slow reader never blocks this
//! thread.
//!
//! Types the program promised are offered like the rest; when a client
//! asks for one, its pipe waits here while the main thread asks the owner
//! for the data (`FromRender::ProvideSelection`), and is written when the
//! answer comes (`ToRender::SelectionData`).
//!
//! Receiving: when another client's selection arrives, its types, and its
//! text read ahead through a pipe serviced by the event loop, land in the
//! shared state `NSPasteboard` reads (see [`crate::clipboard`]). Selections
//! carrying our marker are our own and aren't read back. A read that takes
//! too long is abandoned, its pipe closed, so a client that never finishes
//! doesn't leave pipes behind. The drag pasteboard reads the drag and drop
//! offer under the pointer the same way; [`super::dnd`] follows the drag
//! itself.

use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceData, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferData, DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::{CopyPasteSource, DataSourceData, DataSourceHandler};
use smithay_client_toolkit::data_device_manager::{DataDeviceManagerState, ReadPipe, WritePipe};
use smithay_client_toolkit::globals::GlobalData;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{PostAction, RegistrationToken};
use smithay_client_toolkit::reexports::client::backend::ObjectData;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device::{self, WlDataDevice};
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use smithay_client_toolkit::reexports::client::protocol::wl_data_offer::WlDataOffer;
use smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, delegate_dispatch};

use super::{State, dnd};
use crate::clipboard::{
    Contents, READ_AHEAD_LIMIT, READ_LIMIT, READ_TIMEOUT, ReadAhead, Source, owner_mime, shared, shared_for, text_mime,
};
use crate::protocol::FromRender;

/// How long a read ahead may take before it's abandoned. Longer than the
/// main thread waits: text that comes late is still kept for later reads.
const READ_AHEAD_GIVE_UP: Duration = Duration::from_secs(5);

pub(crate) struct Selection {
    manager: Option<DataDeviceManagerState>,
    devices: Vec<(WlSeat, DataDevice)>,
    /// Our offer and what it holds.
    source: Option<(CopyPasteSource, Arc<Contents>)>,
    /// Contents to offer once there is an input serial to offer them with.
    pending: Option<Option<Arc<Contents>>>,
    /// We cleared the selection; the compositor's word of it is no news.
    cleared_by_us: bool,
    /// Clients' pipes waiting for data the main thread is making, by the
    /// token it answers under.
    promised: std::collections::HashMap<u64, WritePipe>,
    next_token: u64,
    /// Drag and drop under way.
    pub(super) dnd: dnd::Dnd,
}

impl Selection {
    pub fn new(globals: &GlobalList, qh: &QueueHandle<State>) -> Self {
        Selection {
            manager: DataDeviceManagerState::bind(globals, qh).ok(),
            devices: Vec::new(),
            source: None,
            pending: None,
            cleared_by_us: false,
            promised: std::collections::HashMap::new(),
            next_token: 1,
            dnd: dnd::Dnd::default(),
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

/// Read the selection (or the drag and drop offer) as `mime` for the main
/// thread, answering under `token`.
pub(super) fn read(state: &mut State, mime: String, token: u64, source: Source) {
    let devices = state.selection.devices.iter().map(|(_, d)| d.data());
    let pipe = match source {
        Source::Selection => devices
            .filter_map(|d| d.selection_offer())
            .find(|o| o.with_mime_types(|m| m.contains(&mime)))
            .and_then(|o| o.receive(mime).ok()),
        Source::Drag => devices
            .filter_map(|d| d.drag_offer())
            .find(|o| o.with_mime_types(|m| m.contains(&mime)))
            .and_then(|o| o.receive(mime).ok()),
    };
    let shared = shared_for(source);
    let Some(pipe) = pipe else {
        shared.read_arrived(token, None);
        return;
    };
    // The main thread stops waiting after READ_TIMEOUT; a little later the
    // read is abandoned.
    read_pipe(state, pipe, READ_LIMIT, READ_TIMEOUT * 2, move |read| {
        shared.read_arrived(token, if let PipeRead::Done(data) = read { Some(data) } else { None });
    });
}

/// The main thread made promised data for the pipe waiting under `token`.
pub(super) fn provided(state: &mut State, token: u64, data: Option<Arc<[u8]>>) {
    let Some(fd) = state.selection.promised.remove(&token) else { return };
    // Without data the pipe closes here, and the reader gets nothing.
    if let Some(data) = data {
        write_pipe(state, fd, data);
    }
}

/// Write `data` to a client's pipe from the event loop, as much as it
/// takes each time it can.
fn write_pipe(state: &State, fd: WritePipe, data: Arc<[u8]>) {
    // A pipe that polls writable then takes what it has room for without
    // blocking.
    if rustix::io::ioctl_fionbio(&fd, true).is_err() {
        return;
    }
    let mut written = 0;
    let _ = state.loop_handle.insert_source(fd, move |(), file, _| {
        loop {
            if written >= data.len() {
                return PostAction::Remove;
            }
            match (&**file).write(&data[written..]) {
                Ok(n) => written += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => return PostAction::Continue,
                // The reader went away.
                Err(_) => return PostAction::Remove,
            }
        }
    });
}

/// How reading a pipe ended.
enum PipeRead {
    Done(Vec<u8>),
    /// More than the limit came.
    TooLong,
    /// An error, or it took longer than it may.
    Failed,
}

/// Read a pipe to its end, `limit` bytes at most, from the event loop, and
/// hand over what came; after `give_up` the read is abandoned and the pipe
/// closed.
fn read_pipe(state: &State, pipe: ReadPipe, limit: usize, give_up: Duration, done: impl FnOnce(PipeRead) + 'static) {
    // A pipe that polls readable then gives what it has without blocking.
    if rustix::io::ioctl_fionbio(&pipe, true).is_err() {
        done(PipeRead::Failed);
        return;
    }
    let done = std::rc::Rc::new(std::cell::RefCell::new(Some(done)));
    let finish = {
        let done = done.clone();
        move |read: PipeRead| {
            if let Some(done) = done.borrow_mut().take() {
                done(read);
            }
        }
    };
    // The timer and the pipe each end the other.
    let timer: std::rc::Rc<std::cell::Cell<Option<RegistrationToken>>> = Default::default();
    let mut data = Vec::new();
    let finish_read = finish.clone();
    let timer_of_read = timer.clone();
    let reading = state.loop_handle.insert_source(pipe, move |(), file, state| {
        let mut chunk = [0u8; 65536];
        let ended = loop {
            match (&**file).read(&mut chunk) {
                Ok(0) => break Some(PipeRead::Done(std::mem::take(&mut data))),
                Ok(n) if data.len() + n > limit => break Some(PipeRead::TooLong),
                Ok(n) => data.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => break None,
                Err(_) => break Some(PipeRead::Failed),
            }
        };
        let Some(read) = ended else { return PostAction::Continue };
        if let Some(t) = timer_of_read.take() {
            state.loop_handle.remove(t);
        }
        finish_read(read);
        PostAction::Remove
    });
    let Ok(reading) = reading else {
        finish(PipeRead::Failed);
        return;
    };
    let token = state.loop_handle.insert_source(Timer::from_duration(give_up), move |_, _, state| {
        state.loop_handle.remove(reading);
        finish(PipeRead::Failed);
        TimeoutAction::Drop
    });
    timer.set(token.ok());
}

impl DataDeviceHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        dnd::enter(self, device, x, y, surface);
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        dnd::leave(self, device);
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice, x: f64, y: f64) {
        dnd::motion(self, device, x, y);
    }

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        dnd::dropped(self, device);
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        let Some(offer) = device.data::<DataDeviceData>().and_then(|d| d.selection_offer()) else { return };
        let mimes = offer.with_mime_types(|m| m.to_vec());
        if mimes.iter().any(|m| m == owner_mime()) {
            // Our own offer, which the main thread already holds.
            return;
        }
        let text = text_mime(&mimes);
        let Some(read) = shared().foreign_offered(mimes, text.is_some()) else { return };
        let Some(pipe) = text.and_then(|mime| offer.receive(mime).ok()) else {
            shared().text_arrived(read, ReadAhead::Failed);
            return;
        };
        read_pipe(self, pipe, READ_AHEAD_LIMIT, READ_AHEAD_GIVE_UP, move |outcome| {
            let outcome = match outcome {
                PipeRead::Done(data) => ReadAhead::Text(Some(String::from_utf8_lossy(&data).into_owned())),
                PipeRead::TooLong => ReadAhead::TooLong,
                PipeRead::Failed => ReadAhead::Failed,
            };
            shared().text_arrived(read, outcome);
        });
    }
}

impl DataOfferHandler for State {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, offer: &mut DragOffer, actions: DndAction) {
        dnd::source_actions(self, offer, actions);
    }

    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, offer: &mut DragOffer, action: DndAction) {
        dnd::selected_action(self, offer, action);
    }
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
        match data {
            // Without data the pipe closes here, and the reader gets nothing.
            None => {}
            Some(Some(data)) => write_pipe(self, fd, data),
            // Promised: the main thread makes it.
            Some(None) => {
                let token = self.selection.next_token;
                self.selection.next_token += 1;
                self.selection.promised.insert(token, fd);
                self.send(FromRender::ProvideSelection { mime, token });
            }
        }
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
            shared().foreign_offered(Vec::new(), false);
        }
    }

    fn event_created_child(opcode: u16, qh: &QueueHandle<State>) -> Arc<dyn ObjectData> {
        <DataDeviceManagerState as Dispatch<WlDataDevice, DataDeviceData, State>>::event_created_child(opcode, qh)
    }
}

delegate_dispatch!(State: [WlDataDeviceManager: GlobalData] => DataDeviceManagerState);
delegate_dispatch!(State: [WlDataOffer: DataOfferData] => DataDeviceManagerState);
delegate_dispatch!(State: [WlDataSource: DataSourceData] => DataDeviceManagerState);
