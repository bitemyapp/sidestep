//! Drag and drop onto our windows, on the render thread: wl_data_device's
//! enter, motion, leave and drop, and the offer's actions, become messages
//! for the main thread (the toolkit's side: `sidestep-appkit`'s `drag`
//! for AppKit), and its answers become the offer's accepted MIME type and
//! actions, and the finish.
//!
//! Each drag gets a name (a number) that every message about it carries,
//! and every answer names: an answer for a drag that has gone is passed
//! over, and one for the drag under way is its own, whatever order a main
//! thread running nested event loops sends them in.
//!
//! A drag that offers a URL list (files, links) is told to the main thread
//! only once that list is read, so the destination is chosen by what the
//! URLs are (a view that takes files doesn't take a link) and the drag
//! pasteboard has them without asking the source again. The read gives up
//! after [`READ_TIMEOUT`] without data, and then the drag has no URLs.
//!
//! The main thread answers each position before it's sent the next: moves
//! that come while it works are kept, the latest replacing the rest, and
//! sent when the answer comes. So a busy main thread never falls behind
//! the pointer. An answer that changes nothing isn't passed on to the
//! compositor.
//!
//! While a drag is over one of our windows, a timer asks the main thread
//! every [`PERIOD`] to update the destination even if nothing moved
//! (AppKit's periodic `draggingUpdated:`, which scroll views follow to
//! scroll while a drag waits at their edge), whenever it has answered
//! everything else and the destination wants that. The timer goes when the
//! drag does.
//!
//! Crossing from one of a window's surfaces to another (its tiles are
//! subsurfaces) is a leave and an enter with a new offer for the same drag.
//! A leave waits until the batch of events it came in is handled; an enter
//! over the same window in the same batch makes it a move, and the new
//! offer gets the last answer at once, so the destination sees one drag.
//!
//! The data itself is read through `selection::read`, from the drag's own
//! offer, only when the program asks. smithay-client-toolkit destroys a
//! dropped offer when the next drag enters, so a drop the main thread is
//! still handling then can't be finished: its source hears the drop was
//! cancelled, and reads of it come back empty-handed (nil, not empty data).

use std::time::Duration;

use smithay_client_toolkit::data_device_manager::data_device::DataDeviceData;
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferData, DragOffer, receive};
use smithay_client_toolkit::reexports::calloop::RegistrationToken;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
use smithay_client_toolkit::reexports::client::protocol::wl_data_offer::WlDataOffer;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;

use super::selection::{PipeRead, read_pipe};
use super::{Role, State};
use crate::clipboard::{READ_AHEAD_LIMIT, READ_TIMEOUT};
use crate::protocol::{FromRender, WindowId};

#[derive(Default)]
pub struct Dnd {
    current: Option<Drag>,
    /// A leave to tell the main thread once this batch of events is
    /// handled, unless the drag enters the same window again first.
    leaving: Option<Drag>,
    /// The last drag's name.
    named: u64,
    /// The timer for periodic updates, while a drag is over a window.
    ticking: Option<RegistrationToken>,
}

/// How often a drag that waits over a window is updated.
const PERIOD: Duration = Duration::from_millis(50);

struct Drag {
    /// Its name, which messages and answers carry.
    id: u64,
    window: WindowId,
    device: WlDataDevice,
    /// Its offer: the one of the surface it's over.
    offer: WlDataOffer,
    /// The serial of the enter that brought the offer.
    serial: u32,
    mimes: Vec<String>,
    /// What the source allows.
    actions: DndAction,
    /// Where the surface the drag is over sits in the window's content.
    offset: (f64, f64),
    /// The main thread has been told of it (a drag with a URL list waits
    /// for the list).
    told: bool,
    /// Messages the main thread hasn't answered (counting the enter while
    /// it waits to be told).
    outstanding: u32,
    /// The latest position it hasn't been sent.
    pending: Option<(f64, f64)>,
    /// Its last answer: the MIME type accepted, the actions and the one
    /// preferred.
    status: Option<(Option<String>, u32, u32)>,
    /// Whether the destination wants periodic updates.
    periodic: bool,
    /// Dropped: the main thread finishes it.
    dropped: bool,
    /// The action the compositor settled on.
    selected: DndAction,
}

impl Drag {
    /// Tell the offer the last answer.
    fn answer(&self) {
        let Some((mime, actions, preferred)) = self.status.clone() else { return };
        if !self.offer.is_alive() {
            return;
        }
        self.offer.accept(self.serial, mime);
        if self.offer.version() >= 3 {
            self.offer.set_actions(DndAction::from_bits_truncate(actions), DndAction::from_bits_truncate(preferred));
        }
    }
}

/// The drag offer `device` has now.
fn offer(device: &WlDataDevice) -> Option<DragOffer> {
    device.data::<DataDeviceData>()?.drag_offer()
}

/// The window a surface belongs to, and where it sits in the window's
/// content. Decorations are outside the content: a drag over them is
/// placed above it, where no view is.
fn place(state: &State, surface: &WlSurface) -> Option<(WindowId, (f64, f64))> {
    let role = state.role(surface)?;
    let offset = match role {
        Role::Decor(..) => (0.0, -1e6),
        other => state.content_offset(other),
    };
    Some((role.window(), offset))
}

pub(super) fn enter(state: &mut State, device: &WlDataDevice, x: f64, y: f64, surface: &WlSurface) {
    let Some((window, offset)) = place(state, surface) else { return };
    let Some(drag_offer) = offer(device) else { return };
    let position = (x + offset.0, y + offset.1);
    let again = state.selection.dnd.leaving.as_ref().is_some_and(|d| d.window == window);
    if let Some(mut drag) = state.selection.dnd.leaving.take_if(|_| again) {
        // Another surface of the same window: the same drag moving.
        drag.device = device.clone();
        drag.offer = drag_offer.inner().clone();
        drag.serial = drag_offer.serial;
        drag.offset = offset;
        drag.answer();
        state.selection.dnd.current = Some(drag);
        moved(state, position);
        return;
    }
    flush_leave(state);
    let dnd = &mut state.selection.dnd;
    // A drag still here (dropped, and not yet finished) is over.
    dnd.current = None;
    dnd.named += 1;
    let mimes = drag_offer.with_mime_types(|m| m.to_vec());
    let urls = crate::clipboard::url_mime(&mimes);
    let drag = Drag {
        id: dnd.named,
        window,
        device: device.clone(),
        offer: drag_offer.inner().clone(),
        serial: drag_offer.serial,
        mimes,
        actions: drag_offer.source_actions,
        offset,
        told: false,
        outstanding: 1,
        pending: Some(position),
        status: None,
        periodic: false,
        dropped: false,
        selected: DndAction::empty(),
    };
    let (id, wl_offer) = (drag.id, drag.offer.clone());
    dnd.current = Some(drag);
    start_ticking(state);
    let Some(mime) = urls else {
        tell(state, id, None);
        return;
    };
    // The URL list first, then the main thread.
    let Ok(pipe) = receive(&wl_offer, mime.to_owned()) else {
        tell(state, id, None);
        return;
    };
    let handle = state.loop_handle.clone();
    read_pipe(
        state,
        pipe,
        READ_AHEAD_LIMIT,
        READ_TIMEOUT,
        || {},
        move |read| {
            let urls = match read {
                PipeRead::Done(data) => Some((mime.to_owned(), data.into())),
                PipeRead::TooLong | PipeRead::Failed => None,
            };
            // The pipe's callback can't reach the state; the loop's next turn
            // can.
            let _ = handle.insert_idle(move |state| tell(state, id, urls));
        },
    );
}

/// Tell the main thread drag `id` entered, with its URL list, and of its
/// drop if it was dropped meanwhile; nothing if it's gone.
fn tell(state: &mut State, id: u64, urls: Option<(String, std::sync::Arc<[u8]>)>) {
    let Some(drag) = state.selection.dnd.current.as_mut().filter(|d| d.id == id && !d.told) else { return };
    drag.told = true;
    let (x, y) = drag.pending.take().unwrap_or_default();
    let (window, mimes, actions, dropped) = (drag.window, drag.mimes.clone(), drag.actions.bits(), drag.dropped);
    state.send(FromRender::DndEnter { drag: id, window, x, y, mimes, actions, urls });
    if dropped {
        state.send(FromRender::DndDrop { drag: id });
    }
}

/// Update the drag over a window every `PERIOD` while there is one.
fn start_ticking(state: &mut State) {
    if state.selection.dnd.ticking.is_some() {
        return;
    }
    let timer = Timer::from_duration(PERIOD);
    let token = state.loop_handle.insert_source(timer, |_, _, state| {
        let dnd = &mut state.selection.dnd;
        if dnd.current.is_none() && dnd.leaving.is_none() {
            dnd.ticking = None;
            return TimeoutAction::Drop;
        }
        // Not while it's leaving (it may come back) or dropped, only when
        // the main thread has answered everything else (a busy one isn't
        // sent more), and only if the destination wants them.
        let due = dnd.current.as_mut().filter(|d| d.told && !d.dropped && d.outstanding == 0 && d.periodic);
        if let Some(drag) = due {
            drag.outstanding += 1;
            let drag = drag.id;
            state.send(FromRender::DndTick { drag });
        }
        TimeoutAction::ToDuration(PERIOD)
    });
    state.selection.dnd.ticking = token.ok();
}

pub(super) fn motion(state: &mut State, device: &WlDataDevice, x: f64, y: f64) {
    let Some(drag) = state.selection.dnd.current.as_ref().filter(|d| d.device == *device) else { return };
    let position = (x + drag.offset.0, y + drag.offset.1);
    moved(state, position);
}

/// Send `position` if the main thread is ready for it, else keep it.
fn moved(state: &mut State, position: (f64, f64)) {
    let Some(drag) = state.selection.dnd.current.as_mut() else { return };
    if drag.outstanding > 0 {
        drag.pending = Some(position);
        return;
    }
    drag.outstanding += 1;
    let drag = drag.id;
    state.send(FromRender::DndMotion { drag, x: position.0, y: position.1 });
}

pub(super) fn leave(state: &mut State, device: &WlDataDevice) {
    let Some(drag) = state.selection.dnd.current.take_if(|d| d.device == *device) else { return };
    if drag.dropped {
        // The main thread is handling the drop, and finishes it.
        state.selection.dnd.current = Some(drag);
        return;
    }
    flush_leave(state);
    state.selection.dnd.leaving = Some(drag);
    let _ = state.loop_handle.insert_idle(flush_leave);
}

/// Tell the main thread of a leave that no enter over the same window
/// followed (if it was told of the drag at all).
fn flush_leave(state: &mut State) {
    if let Some(drag) = state.selection.dnd.leaving.take()
        && drag.told
    {
        state.send(FromRender::DndLeave { drag: drag.id });
    }
}

pub(super) fn dropped(state: &mut State, device: &WlDataDevice) {
    let selected = offer(device).map(|o| o.selected_action);
    let Some(drag) = state.selection.dnd.current.as_mut().filter(|d| d.device == *device) else { return };
    drag.dropped = true;
    drag.selected = selected.unwrap_or(drag.selected);
    if !drag.told {
        // Told with the enter, once the URL list is read.
        return;
    }
    let id = drag.id;
    // The drop is where the pointer last was.
    if let Some((x, y)) = drag.pending.take() {
        drag.outstanding += 1;
        state.send(FromRender::DndMotion { drag: id, x, y });
    }
    state.send(FromRender::DndDrop { drag: id });
}

/// The drag under way, if `offer` is its offer.
fn of_offer<'a>(state: &'a mut State, offer: &DragOffer) -> Option<&'a mut Drag> {
    state.selection.dnd.current.as_mut().filter(|d| d.offer == *offer.inner())
}

pub(super) fn source_actions(state: &mut State, offer: &DragOffer, actions: DndAction) {
    let Some(drag) = of_offer(state, offer) else { return };
    drag.actions = actions;
    if !drag.told || drag.dropped || offer.inner().version() < 3 {
        // An enter still to come carries them.
        return;
    }
    drag.outstanding += 1;
    let drag = drag.id;
    state.send(FromRender::DndActions { drag, actions: actions.bits() });
}

/// The compositor's choice among the actions both sides allow; AppKit
/// has no use for it before the drop, and the drop takes what the
/// destination answered, if the compositor settled on an action.
pub(super) fn selected_action(state: &mut State, offer: &DragOffer, action: DndAction) {
    if let Some(drag) = of_offer(state, offer) {
        drag.selected = action;
    }
}

/// The main thread's answer about drag `id` to a position, tick or
/// actions it was sent.
pub(super) fn status(state: &mut State, id: u64, mime: Option<String>, actions: u32, preferred: u32, periodic: bool) {
    let dnd = &mut state.selection.dnd;
    // The drag it answers: the current one, or one leaving in this batch,
    // which keeps the answer in case it comes back; else it's gone.
    let current = dnd.current.as_ref().is_some_and(|d| d.id == id);
    let drag = if current { dnd.current.as_mut() } else { dnd.leaving.as_mut().filter(|d| d.id == id) };
    let Some(drag) = drag else { return };
    drag.outstanding = drag.outstanding.saturating_sub(1);
    drag.periodic = periodic;
    let status = Some((mime, actions, preferred));
    let changed = drag.status != status;
    drag.status = status;
    if !current {
        return;
    }
    if changed && !drag.dropped {
        drag.answer();
    }
    let next = if drag.outstanding == 0 { drag.pending.take() } else { None };
    if let Some(position) = next {
        moved(state, position);
    }
}

/// Drag `id`'s drop is over: finish it if the destination took the data,
/// and let the offer go.
pub(super) fn finish(state: &mut State, id: u64, performed: bool) {
    let Some(drag) = state.selection.dnd.current.take_if(|d| d.id == id && d.dropped) else { return };
    if !drag.offer.is_alive() {
        return;
    }
    // Only with an action the compositor settled on when the drop came.
    if performed && !drag.selected.is_empty() && drag.offer.version() >= 3 {
        drag.offer.finish();
    }
    drag.offer.destroy();
}

/// Drag `id`'s offer, to read `mime` from, while it's there to read.
pub(super) fn offer_of(state: &State, id: u64, mime: &str) -> Option<WlDataOffer> {
    let dnd = &state.selection.dnd;
    let drag = [dnd.current.as_ref(), dnd.leaving.as_ref()].into_iter().flatten().find(|d| d.id == id)?;
    let offers = drag.offer.data::<DataOfferData>()?.with_mime_types(|m| m.iter().any(|m| m == mime));
    (offers && drag.offer.is_alive()).then(|| drag.offer.clone())
}
