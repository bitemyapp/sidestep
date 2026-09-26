//! Outputs, for `NSScreen`: the render thread publishes a snapshot of the
//! compositor's outputs (see `crate::screen`) whenever one comes, changes
//! or goes, and tells the main thread which outputs each window's surface
//! is on.
//!
//! A snapshot is published once every output the compositor announced at
//! startup has described itself, so the first `NSScreen.screens` sees them
//! all. Two things wl_output doesn't say are learned from windows: the
//! fractional scale (a window alone on an output gets its preferred scale
//! from wp_fractional_scale_v1) and the work area (a window's
//! xdg_toplevel configure bounds, the size a window can have there without
//! covering the desktop's panels).

use std::cell::RefCell;
use std::collections::HashMap;

use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;

use super::{Role, State};
use crate::protocol::{FromRender, WindowId};
use crate::screen::{self, Output};

#[derive(Default)]
struct Tracker {
    /// Outputs announced at startup that haven't described themselves.
    expected: usize,
    settled: bool,
    /// What windows taught about each output: its fractional scale and
    /// its work area.
    scales: HashMap<u32, f64>,
    bounds: HashMap<u32, (u32, u32)>,
    /// The outputs each window is on, in the order it entered them.
    windows: HashMap<WindowId, Vec<u32>>,
    last: Vec<Output>,
}

thread_local!(static TRACKER: RefCell<Tracker> = RefCell::new(Tracker::default()));

/// The render thread connected: expect the outputs the registry lists.
pub(super) fn started(state: &State, globals: &GlobalList) {
    let expected = globals.contents().with_list(|list| list.iter().filter(|g| g.interface == "wl_output").count());
    TRACKER.with(|t| t.borrow_mut().expected = expected);
    changed(state);
}

/// An output came, changed or went.
pub(super) fn changed(state: &State) {
    let outputs = snapshot(state);
    let publish = TRACKER.with(|t| {
        let mut t = t.borrow_mut();
        if !t.settled && outputs.len() >= t.expected {
            t.settled = true;
        }
        let publish = t.settled && t.last != outputs;
        if publish {
            t.last = outputs.clone();
        }
        publish
    });
    if publish {
        screen::publish(outputs);
        state.send(FromRender::ScreensChanged);
    }
}

/// The main thread asked for the outputs: publish them again if they're
/// settled (a snapshot it missed can't happen, but asking twice is cheap).
pub(super) fn requested(state: &State) {
    if TRACKER.with(|t| t.borrow().settled) {
        screen::publish(snapshot(state));
    }
}

fn snapshot(state: &State) -> Vec<Output> {
    TRACKER.with(|t| {
        let t = t.borrow();
        state
            .outputs
            .outputs()
            .filter_map(|o| state.outputs.info(&o))
            .map(|info| {
                let mode = info.modes.iter().find(|m| m.current).or(info.modes.first());
                let scale = info.scale_factor.max(1);
                let size = info.logical_size.unwrap_or_else(|| {
                    let (w, h) = mode.map_or((0, 0), |m| m.dimensions);
                    (w / scale, h / scale)
                });
                let (x, y) = info.logical_position.unwrap_or(info.location);
                Output {
                    id: info.id,
                    name: info.description.clone().or(info.name.clone()).unwrap_or_else(|| info.model.clone()),
                    rect: (x, y, size.0, size.1),
                    scale: t.scales.get(&info.id).copied().unwrap_or(scale as f64),
                    refresh_mhz: mode.map_or(0, |m| m.refresh_rate),
                    work_area: t.bounds.get(&info.id).copied(),
                }
            })
            .collect()
    })
}

fn output_id(state: &State, output: &WlOutput) -> Option<u32> {
    state.outputs.info(output).map(|i| i.id)
}

/// A window's own surface entered or left an output.
pub(super) fn surface_moved(state: &State, surface: &WlSurface, output: &WlOutput, entered: bool) {
    let (Some(Role::Root(window)), Some(id)) = (state.role(surface), output_id(state, output)) else { return };
    let outputs = TRACKER.with(|t| {
        let mut t = t.borrow_mut();
        let on = t.windows.entry(window).or_default();
        on.retain(|o| *o != id);
        if entered {
            on.push(id);
        }
        on.clone()
    });
    state.send(FromRender::WindowOutputs { window, outputs });
}

/// The window closed.
pub(super) fn window_closed(window: WindowId) {
    TRACKER.with(|t| t.borrow_mut().windows.remove(&window));
}

/// What `window` alone on an output says of it: `scale` is its preferred
/// scale there, or `bounds` its work area.
pub(super) fn learned(state: &State, window: WindowId, scale: Option<f64>, bounds: Option<(u32, u32)>) {
    let news = TRACKER.with(|t| {
        let mut t = t.borrow_mut();
        let Some(&[id]) = t.windows.get(&window).map(Vec::as_slice) else { return false };
        let scale_news = scale.is_some_and(|s| t.scales.insert(id, s) != Some(s));
        let bounds_news = bounds.is_some_and(|b| t.bounds.insert(id, b) != Some(b));
        scale_news || bounds_news
    });
    if news {
        changed(state);
    }
}
