//! The desktop's settings (`sidestep_engine::settings`), applied to
//! AppKit: a change wakes the main thread's run loop, in whatever mode it
//! runs, and every window's views learn their new appearance and redraw
//! ([`apply_changes`]).

use std::sync::Once;

use sidestep_foundation::runloop::{self, Mode};

pub(crate) use sidestep_engine::settings::{deadline, interface, ready};

/// Have changes reach AppKit, then start following the desktop's settings.
pub(crate) fn start() {
    install();
    sidestep_engine::settings::start();
}

/// Have changes reach AppKit: before anything starts the settings thread
/// (the shared application, or the render thread when it starts first).
pub(crate) fn install() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        sidestep_engine::settings::on_change(|| runloop::main().perform(&[Mode::COMMON], apply_changes));
    });
}

/// On the main thread, once a turn: if the desktop's settings changed,
/// every window's views learn their new appearance and redraw.
pub(crate) fn apply_changes() {
    if sidestep_engine::settings::take_changed() {
        crate::appearance::refresh_all();
    }
}
