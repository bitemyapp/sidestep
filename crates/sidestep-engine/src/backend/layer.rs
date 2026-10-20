//! Layer surfaces (wlr-layer-shell): a desktop's own surfaces, its panels,
//! docks, wallpapers and notifications, which the compositor places
//! against an output's edges on a layer of their own rather than as
//! windows (see `protocol::LayerPlacement`).
//!
//! A layer surface is a window's content surface itself, as a popup's is:
//! no frame, no decorations, no window geometry. It takes input as any
//! window's content does (its role is the root's), and its popups are
//! xdg_popups made without an xdg parent and given to it
//! (`get_popup`), as menus opening from a panel are. The compositor
//! configures it with a size, a side of it 0 where the surface may pick
//! (the size it asked for); `closed` (its output went, or the compositor
//! took it away) is a close request, after which it shows nothing.

use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    self, Anchor, KeyboardInteractivity, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};

use super::{Role, Shell, State};
use crate::protocol::{FromRender, Layer, LayerKeyboard, LayerPlacement, WindowId, WindowRequest};

fn layer(l: Layer) -> wlr_layer::Layer {
    match l {
        Layer::Background => wlr_layer::Layer::Background,
        Layer::Bottom => wlr_layer::Layer::Bottom,
        Layer::Top => wlr_layer::Layer::Top,
        Layer::Overlay => wlr_layer::Layer::Overlay,
    }
}

fn keyboard(k: LayerKeyboard) -> KeyboardInteractivity {
    match k {
        LayerKeyboard::None => KeyboardInteractivity::None,
        LayerKeyboard::Exclusive => KeyboardInteractivity::Exclusive,
        LayerKeyboard::OnDemand => KeyboardInteractivity::OnDemand,
    }
}

fn anchors(bits: u8) -> Anchor {
    Anchor::from_bits_truncate(u32::from(bits))
}

/// The output with id `id` (its global's name), if it's there.
pub(super) fn output(state: &State, id: u32) -> Option<WlOutput> {
    state.outputs.outputs().find(|o| state.outputs.info(o).is_some_and(|i| i.id == id))
}

/// `surface` as a layer surface placed as `placement` asks, `width` ×
/// `height` points (0 for the compositor to decide), committed for its
/// first configure; `None` without a layer shell.
pub(super) fn create(
    state: &State,
    surface: &WlSurface,
    width: u32,
    height: u32,
    placement: &LayerPlacement,
) -> Option<LayerSurface> {
    let shell = state.layer_shell.as_ref()?;
    let on = placement.output.and_then(|id| output(state, id));
    let l = shell.create_layer_surface(
        &state.qh,
        surface.clone(),
        layer(placement.layer),
        Some(placement.namespace.clone()),
        on.as_ref(),
    );
    l.set_size(width, height);
    l.set_anchor(anchors(placement.anchors));
    l.set_exclusive_zone(placement.exclusive_zone);
    let [top, right, bottom, left] = placement.margins;
    l.set_margin(top, right, bottom, left);
    l.set_keyboard_interactivity(keyboard(placement.keyboard));
    l.commit();
    Some(l)
}

/// The scale a layer surface on output `id` starts at: the output's.
pub(super) fn initial_scale(state: &State, id: Option<u32>) -> Option<f64> {
    let info = state.outputs.info(&output(state, id?)?)?;
    (info.scale_factor > 0).then_some(f64::from(info.scale_factor))
}

/// A request of a layer surface; whether `window` is one.
pub(super) fn request(state: &mut State, window: WindowId, request: WindowRequest) -> bool {
    let Some(win) = state.windows.get_mut(&window) else { return false };
    let Shell::Layer(l) = &win.shell else { return false };
    match request {
        WindowRequest::Resize(width, height) => {
            if !win.configured {
                (win.width, win.height) = (width, height);
            }
            l.set_size(width, height);
        }
        WindowRequest::Layer(to) => l.set_layer(layer(to)),
        WindowRequest::LayerAnchors(bits) => l.set_anchor(anchors(bits)),
        WindowRequest::LayerExclusiveZone(zone) => l.set_exclusive_zone(zone),
        WindowRequest::LayerMargins([top, right, bottom, left]) => l.set_margin(top, right, bottom, left),
        WindowRequest::LayerKeyboard(k) => l.set_keyboard_interactivity(keyboard(k)),
        // A layer surface is neither moved, minimized, maximized nor
        // full screen, nor activated.
        _ => return true,
    }
    // The compositor answers with a configure.
    l.commit();
    true
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(Role::Root(window)) = self.role(layer.wl_surface()) {
            self.send(FromRender::CloseRequested { window });
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(Role::Root(window)) = self.role(layer.wl_surface()) else { return };
        let Some(win) = self.windows.get(&window) else { return };
        // A side left to the surface is the size it asked for.
        let (width, height) = configure.new_size;
        let width = if width == 0 { win.width } else { width };
        let height = if height == 0 { win.height } else { height };
        self.resize(window, width, height);
    }
}
