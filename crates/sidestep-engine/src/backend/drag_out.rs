//! Drags out of our windows, on the render thread: a wl_data_source
//! offering the main thread's contents, started from the button press
//! under way (its serial, on the window's content), with an icon surface
//! under the pointer drawn from the main thread's ops.
//!
//! A destination's requests are written from the event loop as the
//! clipboard's are (see [`super::selection`]); data the main thread
//! promised is asked of it (`FromRender::ProvideDragData`) while the
//! destination's pipe waits, and written when the answer comes. When the
//! destination finishes the drop, or the drag is cancelled (dropped
//! nowhere that takes it, refused, or replaced by another), the main
//! thread hears how it ended (`FromRender::DragEnded`), and the source and
//! icon go.
//!
//! A drop onto one of our own windows reads the data back through the
//! compositor, which this thread answers: data given up front comes at
//! once, but a promise can't be kept to a main thread waiting for the
//! answer itself.

use std::sync::Arc;

use smithay_client_toolkit::data_device_manager::data_source::DragSource;
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
use smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource;
use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::shm::slot::Buffer;

use super::State;
use crate::clipboard::Contents;
use crate::protocol::{DragIcon, FromRender, WindowId};
use crate::raster;

/// What a source offers as a MIME type: nothing, a promise (`Some(None)`),
/// or its data.
pub(super) type Offered = Option<Option<Arc<[u8]>>>;

/// A drag out of one of our windows, under way.
pub(super) struct Out {
    source: DragSource,
    drag: u64,
    contents: Arc<Contents>,
    /// The icon's surface, and its buffer, kept while the drag shows it.
    icon: Option<(WlSurface, Buffer)>,
    /// The action the compositor last chose with the destination.
    action: DndAction,
}

impl Drop for Out {
    fn drop(&mut self) {
        // The source goes with its handle.
        if let Some((surface, _)) = &self.icon {
            surface.destroy();
        }
    }
}

/// Start dragging `contents` out of `window`, from the latest press.
pub(super) fn start(
    state: &mut State,
    window: WindowId,
    drag: u64,
    contents: Arc<Contents>,
    actions: u32,
    icon: Option<DragIcon>,
) {
    // Another drag out, still there, is over.
    end(state, None, 0);
    let Some((origin, scale)) = state.windows.get(&window).map(|w| (w.surface().clone(), w.scale)) else {
        state.send(FromRender::DragEnded { drag, action: 0 });
        return;
    };
    let icon = icon.and_then(|icon| draw_icon(state, &icon, scale));
    let latest = state.seats.latest_serial();
    let device = latest.as_ref().and_then(|(seat, _)| state.selection.devices.iter().find(|(s, _)| s == seat));
    let (Some(manager), Some((_, device)), Some((_, serial))) = (&state.selection.manager, device, latest) else {
        if let Some((surface, _)) = icon {
            surface.destroy();
        }
        state.send(FromRender::DragEnded { drag, action: 0 });
        return;
    };
    let mimes: Vec<String> = contents.items.iter().map(|(m, _)| m.clone()).collect();
    let source = manager.create_drag_and_drop_source(&state.qh, mimes, DndAction::from_bits_truncate(actions));
    source.start_drag(device, &origin, icon.as_ref().map(|(s, _)| s), serial);
    if let Some((surface, _)) = &icon {
        surface.commit();
    }
    state.selection.out = Some(Out { source, drag, contents, icon, action: DndAction::empty() });
}

/// The icon's surface, `icon`'s ops drawn into a buffer at the window's
/// scale rounded up (a buffer scale), its hotspot at the pointer.
fn draw_icon(state: &mut State, icon: &DragIcon, scale: f64) -> Option<(WlSurface, Buffer)> {
    let s = scale.ceil().clamp(1.0, 8.0) as u32;
    let (w, h) = (icon.width.max(1) * s, icon.height.max(1) * s);
    let mut px = vec![0u32; (w * h) as usize];
    let all = crate::protocol::Rect::new(0.0, 0.0, icon.width as f32, icon.height as f32);
    let mut canvas = raster::Canvas::new(&mut px, w, h, 0.0, s as f32);
    raster::paint(&mut canvas, &mut state.glyphs, &[all], &icon.ops);
    let (buffer, bytes) = state.pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888).ok()?;
    // ARGB8888 is blue, green, red, alpha in memory; canvases hold red,
    // green, blue, alpha, premultiplied as it wants.
    for (dst, src) in super::as_pixels(bytes).iter_mut().zip(&px) {
        *dst = raster::to_xrgb(*src);
    }
    let surface = state.compositor.create_surface(&state.qh);
    surface.set_buffer_scale(s as i32);
    buffer.attach_to(&surface).ok()?;
    surface.damage_buffer(0, 0, w as i32, h as i32);
    if surface.version() >= 5 {
        surface.offset(-icon.hotspot.0, -icon.hotspot.1);
    }
    Some((surface, buffer))
}

/// Whether `source` is the drag out's.
fn ours(state: &State, source: &WlDataSource) -> bool {
    state.selection.out.as_ref().is_some_and(|o| o.source.inner() == source)
}

/// The drag out's data as `mime`, for a destination: `Some(None)` if it's
/// promised, `None` if `source` isn't the drag out's or doesn't offer it;
/// with the drag's name.
pub(super) fn data(state: &State, source: &WlDataSource, mime: &str) -> Option<(u64, Offered)> {
    let out = state.selection.out.as_ref().filter(|o| o.source.inner() == source)?;
    Some((out.drag, out.contents.data(mime)))
}

/// The compositor chose `action` for the drag out with the destination.
pub(super) fn action(state: &mut State, source: &WlDataSource, action: DndAction) {
    if let Some(out) = state.selection.out.as_mut().filter(|o| o.source.inner() == source) {
        out.action = action;
    }
}

/// The drag out (`source`'s, or any for `None`) is over: the destination
/// finished it, taking it with the action chosen last (`performed`), or it
/// was cancelled. Whether it was ours.
pub(super) fn end(state: &mut State, source: Option<&WlDataSource>, performed: u32) -> bool {
    if source.is_some_and(|s| !ours(state, s)) {
        return false;
    }
    let Some(out) = state.selection.out.take() else { return false };
    // Finished with no action chosen (before version 3), a copy.
    let action = if performed != 0 { out.action.bits().max(1) } else { 0 };
    state.send(FromRender::DragEnded { drag: out.drag, action });
    true
}
