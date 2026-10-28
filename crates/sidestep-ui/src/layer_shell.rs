//! A desktop's own surfaces (wlr-layer-shell): panels, docks, menu bars,
//! wallpapers, notifications and on-screen displays, which the
//! compositor places against an output's edges, on a layer above or
//! below the windows, rather than as windows. A program opens one as a
//! window ([`WindowOptions::layer`](crate::WindowOptions::layer)) that
//! takes input and draws as any other, and opens popups from it (a
//! panel's menus); [`Window`](crate::Window)'s `set_layer_…` methods
//! move it while it shows.
//!
//! Its size is in points, as a window's: a side of 0 where it's anchored
//! to both edges that way has the compositor make it as long as the
//! output (less its margins, and other surfaces' exclusive zones), which
//! the handler hears as for any window
//! ([`WindowEvent::Resized`](crate::WindowEvent::Resized)). A compositor
//! that takes a layer surface away (its output went) asks it to close
//! ([`WindowEvent::CloseRequested`](crate::WindowEvent::CloseRequested)),
//! and it shows nothing more (hidden and shown again, it is placed anew);
//! one without wlr-layer-shell (GNOME's) closes it at once.

use sidestep_engine::protocol::{self, LayerPlacement, edge};

/// The layers of a desktop's surfaces, bottom to top.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Under everything: a wallpaper.
    Background,
    /// Over the wallpaper and under windows: desktop icons.
    Bottom,
    /// Over windows: panels, docks, menu bars, notifications.
    Top,
    /// Over everything, full-screen windows included: on-screen displays.
    Overlay,
}

impl Layer {
    pub(crate) fn protocol(self) -> protocol::Layer {
        match self {
            Layer::Background => protocol::Layer::Background,
            Layer::Bottom => protocol::Layer::Bottom,
            Layer::Top => protocol::Layer::Top,
            Layer::Overlay => protocol::Layer::Overlay,
        }
    }
}

/// The edges of its output a layer surface is anchored to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Anchors(u8);

impl Anchors {
    /// None: centred on the output.
    pub const NONE: Anchors = Anchors(0);
    pub const TOP: Anchors = Anchors(edge::TOP);
    pub const BOTTOM: Anchors = Anchors(edge::BOTTOM);
    pub const LEFT: Anchors = Anchors(edge::LEFT);
    pub const RIGHT: Anchors = Anchors(edge::RIGHT);
    /// All four: the whole output, as a wallpaper.
    pub const ALL: Anchors = Anchors(edge::TOP | edge::BOTTOM | edge::LEFT | edge::RIGHT);

    pub fn contains(self, other: Anchors) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for Anchors {
    type Output = Anchors;

    fn bitor(self, other: Anchors) -> Anchors {
        Anchors(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for Anchors {
    fn bitor_assign(&mut self, other: Anchors) {
        self.0 |= other.0;
    }
}

/// Points between a layer surface and the edges it's anchored to (a
/// margin to an edge it isn't anchored to counts for nothing).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Margins {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
}

impl Margins {
    pub const ZERO: Margins = Margins { top: 0, right: 0, bottom: 0, left: 0 };

    /// Clockwise from the top, as CSS has them.
    pub const fn new(top: i32, right: i32, bottom: i32, left: i32) -> Margins {
        Margins { top, right, bottom, left }
    }

    pub const fn all(points: i32) -> Margins {
        Margins::new(points, points, points, points)
    }

    pub(crate) fn protocol(self) -> [i32; 4] {
        [self.top, self.right, self.bottom, self.left]
    }
}

/// When a layer surface has the keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeyboardInteractivity {
    /// Never (the default): a wallpaper, a clock.
    #[default]
    None,
    /// Whenever it's on the top or overlay layer, from every other
    /// surface: a lock screen of old, a launcher's search field.
    Exclusive,
    /// When the compositor gives it, as it gives windows the keyboard (a
    /// click on it): a panel with a search field.
    OnDemand,
}

impl KeyboardInteractivity {
    pub(crate) fn protocol(self) -> protocol::LayerKeyboard {
        match self {
            KeyboardInteractivity::None => protocol::LayerKeyboard::None,
            KeyboardInteractivity::Exclusive => protocol::LayerKeyboard::Exclusive,
            KeyboardInteractivity::OnDemand => protocol::LayerKeyboard::OnDemand,
        }
    }
}

/// Where a layer surface goes and how it behaves: see the module's
/// documentation. Made with [`LayerOptions::new`] and the methods that
/// follow it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LayerOptions {
    pub layer: Layer,
    /// What the surface is (`panel`, `wallpaper`, `notification`), for
    /// the compositor's rules about it.
    pub namespace: String,
    pub anchors: Anchors,
    /// How many points from the edge it's anchored to other surfaces
    /// keep clear of, windows included (a panel's height); 0 to stay out
    /// of others' zones without one of its own; -1 to ignore others'
    /// zones too (a wallpaper). Counted from one edge: anchored to it
    /// alone, or to it and both its neighbours.
    pub exclusive_zone: i32,
    pub margins: Margins,
    pub keyboard: KeyboardInteractivity,
    /// The output it's on, by its [`Output::id`](crate::Output::id);
    /// `None` for the compositor's choice (the focused one, as a rule).
    pub output: Option<u32>,
}

impl LayerOptions {
    /// A surface on `layer`, anchored to nothing (centred), with no
    /// exclusive zone or margins and never the keyboard, on the output
    /// the compositor picks.
    pub fn new(layer: Layer, namespace: impl Into<String>) -> LayerOptions {
        LayerOptions {
            layer,
            namespace: namespace.into(),
            anchors: Anchors::NONE,
            exclusive_zone: 0,
            margins: Margins::ZERO,
            keyboard: KeyboardInteractivity::None,
            output: None,
        }
    }

    pub fn anchors(mut self, anchors: Anchors) -> LayerOptions {
        self.anchors = anchors;
        self
    }

    pub fn exclusive_zone(mut self, points: i32) -> LayerOptions {
        self.exclusive_zone = points;
        self
    }

    pub fn margins(mut self, margins: Margins) -> LayerOptions {
        self.margins = margins;
        self
    }

    pub fn keyboard(mut self, keyboard: KeyboardInteractivity) -> LayerOptions {
        self.keyboard = keyboard;
        self
    }

    /// On output `id` (an [`Output::id`](crate::Output::id)).
    pub fn output(mut self, id: u32) -> LayerOptions {
        self.output = Some(id);
        self
    }

    pub(crate) fn placement(&self) -> LayerPlacement {
        LayerPlacement {
            layer: self.layer.protocol(),
            anchors: self.anchors.bits(),
            exclusive_zone: self.exclusive_zone,
            margins: self.margins.protocol(),
            keyboard: self.keyboard.protocol(),
            namespace: self.namespace.clone(),
            output: self.output,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_name_the_protocols_placement() {
        let options = LayerOptions::new(Layer::Top, "panel")
            .anchors(Anchors::TOP | Anchors::LEFT | Anchors::RIGHT)
            .exclusive_zone(24)
            .margins(Margins::new(1, 2, 3, 4))
            .keyboard(KeyboardInteractivity::OnDemand)
            .output(7);
        let p = options.placement();
        assert_eq!(p.layer, protocol::Layer::Top);
        assert_eq!(p.anchors, edge::TOP | edge::LEFT | edge::RIGHT);
        assert_eq!((p.exclusive_zone, p.margins), (24, [1, 2, 3, 4]));
        assert_eq!((p.keyboard, p.namespace.as_str(), p.output), (protocol::LayerKeyboard::OnDemand, "panel", Some(7)));
        assert!(Anchors::ALL.contains(Anchors::BOTTOM) && !Anchors::TOP.contains(Anchors::ALL));
    }
}
