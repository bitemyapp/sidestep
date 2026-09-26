//! Outputs, for `NSScreen`: the render thread publishes a snapshot of the
//! compositor's outputs (see `crate::screen`) whenever one comes, changes
//! or goes, and tells the main thread which outputs each window's surface
//! is on.
//!
//! A snapshot is published once every output the compositor announced at
//! startup has described itself, so the first `NSScreen.screens` sees them
//! all. wl_output's scale is a whole number, the fractional one rounded
//! up; the fractional one is the output's mode over its logical size (from
//! xdg-output) where those agree with it. Two things are learned from
//! windows: the fractional scale for certain (a window alone on an output
//! gets its preferred scale from wp_fractional_scale_v1) and the work area
//! (a window's xdg_toplevel configure bounds, the size a window can have
//! there without covering the desktop's panels). Each holds while the
//! output keeps the integer scale or the size it had when it was learned,
//! and is forgotten when that changes, until a window there says again.

use std::cell::RefCell;
use std::collections::HashMap;

use smithay_client_toolkit::output::OutputInfo;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;

use super::{Role, State};
use crate::protocol::{FromRender, WindowId};
use crate::screen::{self, Output};

/// A size in logical points.
type Size = (i32, i32);

#[derive(Default)]
struct Tracker {
    /// Outputs announced at startup that haven't described themselves.
    expected: usize,
    settled: bool,
    /// A snapshot was published: the next is a change.
    published: bool,
    /// What windows taught about each output: its fractional scale (with
    /// the integer scale it had then) and its work area (with its size
    /// then).
    scales: HashMap<u32, (i32, f64)>,
    bounds: HashMap<u32, (Size, (u32, u32))>,
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
    let (publish, news) = TRACKER.with(|t| {
        let mut t = t.borrow_mut();
        if !t.settled && outputs.len() >= t.expected {
            t.settled = true;
        }
        let publish = t.settled && t.last != outputs;
        let news = publish && std::mem::replace(&mut t.published, true);
        if publish {
            t.last = outputs.clone();
        }
        (publish, news)
    });
    if publish {
        screen::publish(outputs, news);
        state.send(FromRender::ScreensChanged);
    }
}

/// The main thread asked for the outputs: publish them again if they're
/// settled (a snapshot it missed can't happen, but asking twice is cheap).
pub(super) fn requested(state: &State) {
    if TRACKER.with(|t| t.borrow().settled) {
        screen::publish(snapshot(state), false);
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
                let size = logical_size(&info);
                let (x, y) = info.logical_position.unwrap_or(info.location);
                let learned = t.scales.get(&info.id).filter(|(at, _)| *at == info.scale_factor);
                let work_area = t.bounds.get(&info.id).filter(|(at, _)| *at == size);
                Output {
                    id: info.id,
                    name: info.description.clone().or(info.name.clone()).unwrap_or_else(|| info.model.clone()),
                    rect: (x, y, size.0, size.1),
                    scale: learned.map_or_else(|| scale_of(&info), |(_, s)| *s),
                    refresh_mhz: mode.map_or(0, |m| m.refresh_rate),
                    work_area: work_area.map(|(_, b)| *b),
                }
            })
            .collect()
    })
}

/// An output's scale, as far as wl_output and xdg-output tell.
fn scale_of(info: &OutputInfo) -> f64 {
    let mode = info.modes.iter().find(|m| m.current).or(info.modes.first()).map(|m| m.dimensions);
    let rotated =
        matches!(info.transform, Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270);
    derived_scale(info.scale_factor, mode, info.logical_size, rotated)
}

/// The scale an output of mode `mode` (in pixels, turned a quarter if
/// `rotated`) and logical size `logical` has, to the 120th as
/// wp_fractional_scale_v1 counts, if wl_output's `integer` scale is it
/// rounded up; else `integer`.
fn derived_scale(integer: i32, mode: Option<Size>, logical: Option<Size>, rotated: bool) -> f64 {
    let integer = integer.max(1) as f64;
    let (Some((w, h)), Some((logical_w, _))) = (mode, logical) else { return integer };
    let width = if rotated { h } else { w };
    if width <= 0 || logical_w <= 0 {
        return integer;
    }
    let scale = (width as f64 / logical_w as f64 * 120.0).round() / 120.0;
    if scale > integer - 1.0 && scale <= integer { scale } else { integer }
}

/// An output's size in logical points: as xdg-output says, else its mode
/// over its scale.
fn logical_size(info: &OutputInfo) -> Size {
    info.logical_size.unwrap_or_else(|| {
        let mode = info.modes.iter().find(|m| m.current).or(info.modes.first());
        let (w, h) = mode.map_or((0, 0), |m| m.dimensions);
        let scale = info.scale_factor.max(1);
        (w / scale, h / scale)
    })
}

/// An output went. It's still in the list while this runs, so the new
/// snapshot waits until it's gone; windows on it are told first.
pub(super) fn destroyed(state: &mut State, output: &WlOutput) {
    let Some(id) = output_id(state, output) else { return };
    let moved: Vec<(WindowId, Vec<u32>)> = TRACKER.with(|t| {
        let mut t = t.borrow_mut();
        t.scales.remove(&id);
        t.bounds.remove(&id);
        let on_it = t.windows.iter_mut().filter(|(_, on)| on.contains(&id));
        on_it
            .map(|(window, on)| {
                on.retain(|o| *o != id);
                (*window, on.clone())
            })
            .collect()
    });
    for (window, outputs) in moved {
        state.send(FromRender::WindowOutputs { window, outputs });
    }
    let _ = state.loop_handle.insert_idle(|state| changed(state));
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
    let alone_on = TRACKER.with(|t| match t.borrow().windows.get(&window).map(Vec::as_slice) {
        Some(&[id]) => Some(id),
        _ => None,
    });
    let Some(id) = alone_on else { return };
    // What the output is now, which what's learned holds for.
    let Some(info) = state.outputs.outputs().filter_map(|o| state.outputs.info(&o)).find(|i| i.id == id) else {
        return;
    };
    let (integer_scale, size) = (info.scale_factor, logical_size(&info));
    let news = TRACKER.with(|t| {
        let mut t = t.borrow_mut();
        let scale_news = scale.is_some_and(|s| t.scales.insert(id, (integer_scale, s)) != Some((integer_scale, s)));
        let bounds_news = bounds.is_some_and(|b| t.bounds.insert(id, (size, b)) != Some((size, b)));
        scale_news || bounds_news
    });
    if news {
        changed(state);
    }
}

#[cfg(test)]
mod tests {
    use super::derived_scale;

    #[test]
    fn fractional_scales_from_logical_sizes() {
        // sway at 1.5: 1920x1200 pixels, 1280x800 points, wl_output says 2.
        assert_eq!(derived_scale(2, Some((1920, 1200)), Some((1280, 800)), false), 1.5);
        assert_eq!(derived_scale(2, Some((2560, 1440)), Some((2048, 1152)), false), 1.25);
        assert_eq!(derived_scale(2, Some((2560, 1440)), Some((1920, 1080)), false), 160.0 / 120.0);
        // Integer scales, and a turned output.
        assert_eq!(derived_scale(1, Some((1280, 800)), Some((1280, 800)), false), 1.0);
        assert_eq!(derived_scale(2, Some((2560, 1600)), Some((800, 1280)), true), 2.0);
        // What doesn't agree with wl_output, or isn't known, is its scale.
        assert_eq!(derived_scale(2, Some((1920, 1080)), Some((1920, 1080)), false), 2.0);
        assert_eq!(derived_scale(3, Some((1920, 1080)), None, false), 3.0);
        assert_eq!(derived_scale(0, None, None, false), 1.0);
    }
}
