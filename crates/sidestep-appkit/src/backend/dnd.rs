//! Drag and drop onto our windows, on the render thread: wl_data_device's
//! enter, motion, leave and drop, and the offer's actions, become messages
//! for the main thread (`crate::drag` runs AppKit's side), and its answers
//! become the offer's accepted MIME type and actions, and the finish.
//!
//! The main thread answers each position before it's sent the next: moves
//! that come while it works are kept, the latest replacing the rest, and
//! sent when the answer comes. So a busy main thread never falls behind
//! the pointer.
//!
//! Crossing from one of a window's surfaces to another (its tiles are
//! subsurfaces) is a leave and an enter with a new offer for the same drag.
//! A leave waits until the batch of events it came in is handled; an enter
//! over the same window in the same batch makes it a move, and the new
//! offer gets the last answer at once, so the destination sees one drag.
//!
//! The data itself is read through `selection::read`, from the offer under
//! the pointer, only when the program asks.

use smithay_client_toolkit::data_device_manager::data_device::DataDeviceData;
use smithay_client_toolkit::data_device_manager::data_offer::DragOffer;
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;

use super::{Role, State};
use crate::protocol::{FromRender, WindowId};

#[derive(Default)]
pub(crate) struct Dnd {
    current: Option<Drag>,
    /// A leave to tell the main thread once this batch of events is
    /// handled, unless the drag enters the same window again first.
    leaving: Option<Drag>,
}

struct Drag {
    window: WindowId,
    device: WlDataDevice,
    /// Where the surface the drag is over sits in the window's content.
    offset: (f64, f64),
    /// Messages the main thread hasn't answered.
    outstanding: u32,
    /// The latest position it hasn't been sent.
    pending: Option<(f64, f64)>,
    /// Its last answer: the MIME type accepted, the actions and the one
    /// preferred.
    status: Option<(Option<String>, u32, u32)>,
    dropped: bool,
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
        drag.offset = offset;
        if let Some((mime, actions, preferred)) = drag.status.clone() {
            answer(&drag_offer, mime, actions, preferred);
        }
        state.selection.dnd.current = Some(drag);
        moved(state, position);
        return;
    }
    flush_leave(state);
    let mimes = drag_offer.with_mime_types(|m| m.to_vec());
    let actions = drag_offer.source_actions.bits();
    state.send(FromRender::DndEnter { window, x: position.0, y: position.1, mimes, actions });
    state.selection.dnd.current = Some(Drag {
        window,
        device: device.clone(),
        offset,
        outstanding: 1,
        pending: None,
        status: None,
        dropped: false,
    });
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
    state.send(FromRender::DndMotion { x: position.0, y: position.1 });
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
/// followed.
fn flush_leave(state: &mut State) {
    if state.selection.dnd.leaving.take().is_some() {
        state.send(FromRender::DndLeave);
    }
}

pub(super) fn dropped(state: &mut State, device: &WlDataDevice) {
    let Some(drag) = state.selection.dnd.current.as_mut().filter(|d| d.device == *device) else { return };
    drag.dropped = true;
    // The drop is where the pointer last was.
    if let Some((x, y)) = drag.pending.take() {
        state.send(FromRender::DndMotion { x, y });
    }
    state.send(FromRender::DndDrop);
}

pub(super) fn source_actions(state: &mut State, offer: &DragOffer, actions: DndAction) {
    let Some(drag) = state.selection.dnd.current.as_mut() else { return };
    if drag.dropped || offer.inner().version() < 3 {
        return;
    }
    drag.outstanding += 1;
    state.send(FromRender::DndActions { actions: actions.bits() });
}

/// The compositor's choice among the actions both sides allow; AppKit
/// has no use for it before the drop, and the drop takes what the
/// destination answered.
pub(super) fn selected_action(_: &mut State, _: &DragOffer, _: DndAction) {}

/// The main thread's answer to the latest position or actions.
pub(super) fn status(state: &mut State, mime: Option<String>, actions: u32, preferred: u32) {
    let Some(drag) = state.selection.dnd.current.as_mut() else { return };
    drag.outstanding = drag.outstanding.saturating_sub(1);
    drag.status = Some((mime.clone(), actions, preferred));
    if !drag.dropped
        && let Some(offer) = offer(&drag.device)
    {
        answer(&offer, mime, actions, preferred);
    }
    if drag.outstanding == 0
        && let Some(position) = drag.pending.take()
    {
        moved(state, position);
    }
}

fn answer(offer: &DragOffer, mime: Option<String>, actions: u32, preferred: u32) {
    offer.accept_mime_type(offer.serial, mime);
    offer.set_actions(DndAction::from_bits_truncate(actions), DndAction::from_bits_truncate(preferred));
}

/// The drop is over: finish it if the destination took the data, and let
/// the offer go.
pub(super) fn finish(state: &mut State, performed: bool) {
    let Some(drag) = state.selection.dnd.current.take() else { return };
    let Some(offer) = offer(&drag.device) else { return };
    if performed && offer.dropped && !offer.selected_action.is_empty() {
        offer.finish();
    }
    offer.destroy();
}
