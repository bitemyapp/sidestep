//! The desktop's settings (`sidestep_engine::settings`), applied to
//! AppKit: a change wakes the main thread's run loop, in whatever mode it
//! runs, and every window's views learn their new appearance and redraw
//! ([`apply_changes`]).
//!
//! Programs also learn of the desktop's light or dark as macOS tells them:
//! `NSGlobalDomain`'s `AppleInterfaceStyle` is "Dark" while it is dark,
//! a switch between the two posts `AppleInterfaceThemeChangedNotification`
//! to the program's distributed notification observers, and the
//! application's `effectiveAppearance`, while it has none of its own,
//! changes observably (KVO; an observer asking for the old value gets the
//! new one, as the desktop changed before the main thread heard).

use std::cell::Cell;
use std::sync::Once;

use sidestep_foundation::runloop::{self, Mode};

pub(crate) use sidestep_engine::settings::{deadline, interface, ready};

use crate::appearance::Id;

/// `AppleInterfaceThemeChangedNotification`.
const THEME_CHANGED: &str = "AppleInterfaceThemeChangedNotification";

thread_local! {
    /// The desktop's appearance as the main thread last applied it: none
    /// before the first change, when it reads light.
    static APPLIED: Cell<Option<Id>> = const { Cell::new(None) };
}

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
        sidestep_foundation::set_desktop_defaults(desktop_defaults);
        sidestep_engine::settings::on_change(|| runloop::main().perform(&[Mode::COMMON], apply_changes));
    });
}

/// What macOS's system settings keep in `NSGlobalDomain` that programs
/// read: `AppleInterfaceStyle`, "Dark" while the desktop is dark.
fn desktop_defaults() -> Vec<(&'static str, &'static str)> {
    if crate::appearance::system().look().dark() { vec![("AppleInterfaceStyle", "Dark")] } else { Vec::new() }
}

/// On the main thread, once a turn: if the desktop's settings changed,
/// every window's views learn their new appearance and redraw, and if its
/// appearance changed, the program hears as macOS would tell it.
pub(crate) fn apply_changes() {
    if !sidestep_engine::settings::take_changed() {
        return;
    }
    let now = crate::appearance::system();
    let before = APPLIED.with(|a| a.replace(Some(now))).unwrap_or(Id::Aqua);
    let app = crate::app::existing().filter(|_| crate::appearance::app_appearance().is_none() && before != now);
    if let Some(app) = &app {
        sidestep_foundation::observing::will_change(app, "effectiveAppearance");
    }
    crate::appearance::refresh_all();
    if let Some(app) = &app {
        sidestep_foundation::observing::did_change(app, "effectiveAppearance");
    }
    if before.look().dark() != now.look().dark() {
        sidestep_foundation::post_from_desktop(THEME_CHANGED);
    }
}
