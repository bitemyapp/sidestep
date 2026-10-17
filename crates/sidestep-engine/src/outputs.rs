//! The compositor's outputs, as the render thread last described them.
//!
//! The render thread publishes a snapshot of the outputs (see
//! `backend::outputs`) as they come, change and go; toolkits read the
//! latest when they're asked, waiting for the first if they must. The
//! first question, before any window, starts the render thread
//! ([`snapshot`]'s `start`) and waits for the outputs (a round trip or two
//! with the compositor, bounded by [`FIRST_SNAPSHOT`]). Without a Wayland
//! display there are none.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long the first question about outputs waits for the compositor to
/// describe them.
pub const FIRST_SNAPSHOT: Duration = Duration::from_secs(1);

/// An output, as the render thread publishes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    /// The wl_output global's name, which stays the same while it's there.
    pub id: u32,
    /// A name to show: its description, else its connector's, else its
    /// model.
    pub name: String,
    /// Where it is in the compositor's space, in logical points (y down).
    pub rect: (i32, i32, i32, i32),
    /// Its scale, fractional where that's known (see `backend::outputs`).
    pub scale: f64,
    pub refresh_mhz: i32,
    /// The largest window size there that leaves the desktop's panels
    /// uncovered, once a window was told it.
    pub work_area: Option<(u32, u32)>,
    /// Its connector's name (wl_output's `name`: `eDP-1`, `HDMI-A-1`),
    /// which stays the same from one session to the next, and the
    /// compositor's description of it, where the compositor says them.
    pub connector: Option<String>,
    pub description: Option<String>,
    pub make: String,
    pub model: String,
    /// wl_output's scale: a whole number, a fractional scale rounded up.
    pub integer_scale: i32,
    /// Its size in millimetres; 0 where it isn't known (a projector).
    pub physical_size: (i32, i32),
}

#[derive(Default)]
struct Published {
    /// The render thread has described every output it knew of at start.
    settled: bool,
    /// Counts snapshots, so readers know when to update.
    generation: u64,
    /// Some snapshot since the first was a change (a program is told of
    /// changes even if it never asked about the outputs before them).
    changed: bool,
    outputs: Vec<Output>,
    /// A reader asked the render thread to start for them.
    asked: Option<Instant>,
}

static PUBLISHED: Mutex<Published> =
    Mutex::new(Published { settled: false, generation: 0, changed: false, outputs: Vec::new(), asked: None });
static ARRIVED: Condvar = Condvar::new();

fn published() -> std::sync::MutexGuard<'static, Published> {
    PUBLISHED.lock().unwrap_or_else(|e| e.into_inner())
}

/// The render thread's side: the outputs are now these; `news` unless
/// they're the first, or the same again.
pub fn publish(outputs: Vec<Output>, news: bool) {
    let mut p = published();
    p.settled = true;
    p.generation += 1;
    p.changed |= news;
    p.outputs = outputs;
    drop(p);
    ARRIVED.notify_all();
}

/// The latest snapshot if it's newer than snapshot `seen` (0 for none
/// yet), with its generation. Before the first, `start` asks the render
/// thread to publish (`ToRender::PublishOutputs`, starting it), and this
/// waits for it up to [`FIRST_SNAPSHOT`] from the first time anyone asked.
pub fn snapshot(seen: u64, start: impl FnOnce()) -> Option<(u64, Vec<Output>)> {
    let mut p = published();
    if !p.settled {
        let display = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"].iter().any(|v| std::env::var_os(v).is_some());
        if !display {
            return None;
        }
        let asked = *p.asked.get_or_insert_with(Instant::now);
        drop(p);
        start();
        p = published();
        while !p.settled {
            let left = (asked + FIRST_SNAPSHOT).saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            p = ARRIVED.wait_timeout(p, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
    (p.generation != seen).then(|| (p.generation, p.outputs.clone()))
}

/// The latest snapshot, without waiting: none before the render thread
/// first described the outputs (it does by itself once it connects, and
/// tells the main thread with `FromRender::ScreensChanged`).
pub fn latest() -> Option<Vec<Output>> {
    let p = published();
    p.settled.then(|| p.outputs.clone())
}

/// Whether some snapshot since the first was a change.
pub fn changed_since_first() -> bool {
    published().changed
}
