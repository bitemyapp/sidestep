//! The desktop's settings: light or dark, high contrast and the accent
//! color, and (on GNOME) the text scale, the interface and monospaced
//! fonts, the cursor size and whether to animate, followed as they change.
//!
//! One thread, `sidestep-settings`, asks xdg-desktop-portal's Settings
//! interface over the session bus (`desktop`, a minimal D-Bus client) and
//! waits for changes; it's the program's only D-Bus connection. It starts
//! with the shared application, alongside the Wayland connection, so its
//! answer is usually in before the first window shows. What it learns goes
//! into the appearance's and palette's shared state (atomics and a lock
//! read when colors resolve); it tells the render thread (decorations) and
//! wakes the main thread, which redraws every window once
//! ([`apply_changes`]).
//!
//! Nothing waits on it. A window's first frame is held back, the event
//! loop still running, until the first answer or 150 ms after the thread
//! started, so a dark desktop doesn't flash a light window. Without a
//! session bus or a portal the answer is light at once.
//! `SIDESTEP_APPEARANCE=light` or `dark` (else `SIDESTEP_THEME`, or a dark
//! `GTK_THEME`) and `SIDESTEP_ACCENT=#rrggbb` override the desktop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::desktop::{Scheme, Setting};

/// How long a first frame waits for the desktop's answer.
const FIRST_ANSWER: Duration = Duration::from_millis(150);

static STARTED: OnceLock<Instant> = OnceLock::new();
/// The settings changed since the main thread last looked.
static CHANGED: AtomicBool = AtomicBool::new(false);
/// Where to tell the main and render threads, once they run.
static WAKE: Mutex<Option<Wake>> = Mutex::new(None);
/// The desktop's interface settings, as last told.
static INTERFACE: Mutex<Interface> = Mutex::new(Interface::DEFAULT);

/// The desktop's interface settings (GNOME's `org.gnome.desktop.interface`,
/// through the portal), for the text engine's system fonts, cursors and
/// reduced motion. Unknown values keep these defaults.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Interface {
    /// How much larger than their size text is shown.
    pub text_scale: f64,
    /// Fonts as GNOME names them (`Cantarell 11`).
    pub font: Option<String>,
    pub monospace_font: Option<String>,
    pub cursor_size: Option<u32>,
    /// False when the user asked for less motion.
    pub animations: bool,
}

impl Interface {
    const DEFAULT: Interface =
        Interface { text_scale: 1.0, font: None, monospace_font: None, cursor_size: None, animations: true };
}

/// The desktop's interface settings as last told.
#[allow(dead_code, reason = "for the text engine and cursors to read")]
pub(crate) fn interface() -> Interface {
    INTERFACE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Record an interface setting; true if it changed.
fn set_interface(f: impl FnOnce(&mut Interface)) -> bool {
    let mut i = INTERFACE.lock().unwrap_or_else(|e| e.into_inner());
    let old = i.clone();
    f(&mut i);
    *i != old
}

struct Wake {
    render: smithay_client_toolkit::reexports::calloop::channel::Sender<crate::protocol::ToRender>,
}

/// What the environment says, which wins over the desktop.
fn overrides() -> (Option<bool>, Option<[f64; 3]>) {
    let dark = match std::env::var("SIDESTEP_APPEARANCE").as_deref() {
        Ok("dark") => Some(true),
        Ok("light") => Some(false),
        _ => crate::backend::explicit_theme(),
    };
    let accent = std::env::var("SIDESTEP_ACCENT").ok().and_then(|v| parse_hex(&v));
    (dark, accent)
}

fn parse_hex(v: &str) -> Option<[f64; 3]> {
    let v = v.trim().trim_start_matches('#');
    if v.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(v, 16).ok()?;
    Some([(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff].map(|c| f64::from(c) / 255.0))
}

/// Start following the desktop's settings, once.
pub(crate) fn start() {
    if STARTED.set(Instant::now()).is_err() {
        return;
    }
    let (dark, accent) = overrides();
    if let Some(accent) = accent {
        crate::palette::set_accent(Some([accent[0] as f32, accent[1] as f32, accent[2] as f32, 1.0]));
    }
    if let Some(dark) = dark {
        crate::appearance::set_system(dark, false);
    }
    let state = Mutex::new((dark.unwrap_or(false), false));
    crate::desktop::watch(
        move |setting| {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            let changed = match setting {
                Setting::Scheme(scheme) if dark.is_none() => {
                    s.0 = scheme == Scheme::Dark;
                    crate::appearance::set_system(s.0, s.1)
                }
                Setting::Contrast(high) if dark.is_none() => {
                    s.1 = high;
                    crate::appearance::set_system(s.0, s.1)
                }
                Setting::Accent(color) if accent.is_none() => {
                    crate::palette::set_accent(color.map(|c| [c[0] as f32, c[1] as f32, c[2] as f32, 1.0]))
                }
                Setting::TextScale(v) => set_interface(|i| i.text_scale = v),
                Setting::Font(f) => set_interface(|i| i.font = Some(f)),
                Setting::MonospaceFont(f) => set_interface(|i| i.monospace_font = Some(f)),
                Setting::CursorSize(v) => set_interface(|i| i.cursor_size = u32::try_from(v).ok().filter(|v| *v > 0)),
                Setting::Animations(on) => set_interface(|i| i.animations = on),
                _ => false,
            };
            if changed {
                notify(s.0);
            }
        },
        move || {
            // No answer (no bus, no portal) is an answer: light.
            if !crate::appearance::system_known() {
                crate::appearance::set_system(false, false);
            }
            notify(crate::appearance::system().look().dark());
        },
    );
}

/// Tell the main thread (and the render thread, for decorations) that the
/// settings changed.
fn notify(dark: bool) {
    CHANGED.store(true, Ordering::Release);
    if let Some(wake) = WAKE.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        && crate::backend::explicit_theme().is_none()
    {
        let _ = wake.render.send(crate::protocol::ToRender::ColorScheme { dark });
    }
    // The main thread applies it on its run loop, in whatever mode it runs.
    sidestep_foundation::runloop::main().perform(&[sidestep_foundation::runloop::Mode::COMMON], apply_changes);
}

/// The render thread started: from now on, changes reach it and wake the
/// main thread. Tells it the current preference at once.
pub(crate) fn connect(render: smithay_client_toolkit::reexports::calloop::channel::Sender<crate::protocol::ToRender>) {
    start();
    if crate::appearance::system_known() && crate::backend::explicit_theme().is_none() {
        let _ = render.send(crate::protocol::ToRender::ColorScheme { dark: crate::appearance::system().look().dark() });
    }
    *WAKE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Wake { render });
}

/// Whether a first frame may show: the desktop answered, or waiting
/// longer would be noticed.
pub(crate) fn ready() -> bool {
    crate::appearance::system_known() || STARTED.get().is_none_or(|t| t.elapsed() >= FIRST_ANSWER)
}

/// When the main loop should look again for a first frame held back: the
/// end of the wait, while it's still ahead. After it, a late answer wakes
/// the loop itself ([`notify`]), so there's nothing to wait for.
pub(crate) fn deadline() -> Option<Instant> {
    wait_until(STARTED.get().copied(), crate::appearance::system_known(), Instant::now())
}

fn wait_until(started: Option<Instant>, answered: bool, now: Instant) -> Option<Instant> {
    started.filter(|_| !answered).map(|t| t + FIRST_ANSWER).filter(|d| *d > now)
}

/// On the main thread, once a turn: if the desktop's settings changed,
/// every window's views learn their new appearance and redraw.
pub(crate) fn apply_changes() {
    if CHANGED.swap(false, Ordering::Acquire) {
        crate::appearance::refresh_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frames_wait_only_until_the_deadline() {
        let t = Instant::now();
        assert_eq!(wait_until(Some(t), false, t), Some(t + FIRST_ANSWER));
        assert_eq!(wait_until(Some(t), true, t), None, "answered");
        assert_eq!(wait_until(None, false, t), None, "not started");
        // Past the deadline there's nothing to wait for: a zero timeout
        // would spin the main loop.
        assert_eq!(wait_until(Some(t), false, t + FIRST_ANSWER), None);
        assert_eq!(wait_until(Some(t), false, t + FIRST_ANSWER * 3), None);
    }

    #[test]
    fn accents_parse_from_hex() {
        assert_eq!(parse_hex("#3584e4"), Some([0x35 as f64 / 255.0, 0x84 as f64 / 255.0, 0xe4 as f64 / 255.0]));
        assert_eq!(parse_hex("ff0000"), Some([1.0, 0.0, 0.0]));
        assert_eq!(parse_hex("#fff"), None);
    }
}
