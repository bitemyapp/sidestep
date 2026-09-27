//! A render thread without a display, for tests: `SIDESTEP_BACKEND=null`
//! (or `testing::use_null_backend`) starts it instead of the Wayland one.
//!
//! It answers as a compositor would, at once and always the same way: a
//! new window is configured at the size asked for, at scale 1, and a new
//! toplevel gets the keyboard; an activation request gives the window the
//! keyboard; a resize, maximize or full-screen request is configured; each
//! present is shown at once (a frame). Drawing is dropped, but for the
//! colors and places of the text painted while a test asks for them
//! (`testing::note_painted_text`). Everything else the main thread asks is
//! written down for tests to read (`testing::take_render_log`), and input
//! comes only from tests, which add messages to the main thread's inbox
//! directly (`testing`), so a test decides exactly what the program sees
//! and when.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};

use smithay_client_toolkit::reexports::calloop::channel::{self, Channel};

use super::Backend;
use crate::event_loop::MainSender;
use crate::protocol::{FromRender, Op, ToRender, WindowId, WindowRequest, WindowState};
use crate::testing::{PaintedText, Seen};
use sidestep_foundation::runloop::SourceSignal;

/// Asked for by a test, before the render thread started.
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// This render thread runs (so sends are counted).
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Messages the main thread sent, and those this thread has handled
/// (after sending its answers), so a test can wait for the answers.
pub(crate) static SENT: AtomicU64 = AtomicU64::new(0);
pub(crate) static HANDLED: AtomicU64 = AtomicU64::new(0);

/// The main thread is about to send a message: counted while this render
/// thread runs; a load that is never set otherwise.
#[inline]
pub(crate) fn sending() {
    if RUNNING.load(Ordering::Relaxed) {
        SENT.fetch_add(1, Ordering::Release);
    }
}

/// What the main thread asked for, oldest first.
pub(crate) static LOG: Mutex<Vec<Seen>> = Mutex::new(Vec::new());

/// The text painted, oldest first, while a test notes it.
pub(crate) static TEXT: Mutex<Option<Vec<PaintedText>>> = Mutex::new(None);

/// Core Animation's layer trees as this render thread has them (see
/// `quartzcore::tree`), for tests to look at.
pub(crate) static CA: Mutex<Option<crate::quartzcore::tree::Compositor>> = Mutex::new(None);

/// Windows' pixels, at scale 1, while a test captures them
/// (`testing::capture_pixels`): what their paints drew, composites
/// included.
pub(crate) static PIXELS: Mutex<Option<HashMap<WindowId, Captured>>> = Mutex::new(None);

/// A window's captured width, height and pixels.
pub(crate) type Captured = (u32, u32, Vec<u32>);

fn with_ca<R>(f: impl FnOnce(&mut crate::quartzcore::tree::Compositor) -> R) -> R {
    let mut ca = CA.lock().unwrap_or_else(|e| e.into_inner());
    f(ca.get_or_insert_with(Default::default))
}

/// Draw `ops` into a captured window's pixels.
fn capture(window: WindowId, rects: &[crate::protocol::Rect], ops: &[Op], glyphs: &mut crate::raster::Glyphs) {
    let mut pixels = PIXELS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(all) = pixels.as_mut() else { return };
    let Some((w, h, px)) = all.get_mut(&window) else { return };
    let mut canvas = crate::raster::Canvas::new(px, *w, *h, 0.0, 1.0);
    crate::raster::paint(&mut canvas, glyphs, rects, ops);
}

/// A window's frame of its layer trees at `at`, drawn into its captured
/// pixels.
fn composite(window: WindowId, at: Option<f64>, glyphs: &mut crate::raster::Glyphs) {
    let redraws = with_ca(|ca| {
        let clock = std::mem::replace(&mut ca.clock, at);
        let now = ca.now();
        let (redraws, _) = ca.frame(window, now);
        ca.clock = clock;
        redraws
    });
    for r in redraws {
        if r.target == crate::protocol::Target::Root {
            capture(window, &r.rects, &r.ops, glyphs);
        }
    }
}

pub(crate) fn request() {
    REQUESTED.store(true, Ordering::Relaxed);
}

/// Whether to start this backend rather than Wayland's.
pub(crate) fn chosen() -> bool {
    REQUESTED.load(Ordering::Relaxed) || std::env::var("SIDESTEP_BACKEND").is_ok_and(|v| v == "null")
}

pub(crate) fn start(wake: SourceSignal) -> Backend {
    RUNNING.store(true, Ordering::Relaxed);
    let (tx, channel) = channel::channel();
    let (to_main, rx) = mpsc::channel();
    let to_main = MainSender::new(to_main, wake);
    std::thread::Builder::new()
        .name("sidestep-null-render".into())
        .spawn(move || run(channel, to_main))
        .expect("sidestep: couldn't start the render thread");
    Backend { tx, rx }
}

struct Win {
    width: u32,
    height: u32,
    state: WindowState,
    /// The menu bar's height, part of the title bar.
    bar: u32,
}

fn run(channel: Channel<ToRender>, to_main: MainSender) {
    let mut windows: HashMap<WindowId, Win> = HashMap::new();
    let mut glyphs = crate::raster::Glyphs::default();
    let mut focused: Option<WindowId> = None;
    let send = |msg| {
        let _ = to_main.send(msg);
    };
    let configure = |window: WindowId, win: &Win| FromRender::Configure {
        window,
        width: win.width,
        height: win.height,
        scale: 1.0,
        titlebar: win.bar,
        state: win.state,
    };
    let focus = |focused: &mut Option<WindowId>, window: Option<WindowId>| {
        if *focused == window {
            return;
        }
        if let Some(old) = focused.take() {
            send(FromRender::Focus { window: old, focused: false });
        }
        if let Some(new) = window {
            send(FromRender::Focus { window: new, focused: true });
        }
        *focused = window;
    };
    while let Ok(msg) = channel.recv() {
        match msg {
            ToRender::CreateWindow { window, width, height, popup, sheet_of, .. } => {
                if let Some(all) = PIXELS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    all.insert(window, (width, height, vec![0; width as usize * height as usize]));
                }
                let win = Win { width, height, state: WindowState::default(), bar: 0 };
                send(configure(window, &win));
                windows.insert(window, win);
                note(Seen::Created { window, width, height, popup_of: popup.map(|p| p.parent), sheet_of });
                // Popups and sheets are parts of other windows: the
                // keyboard stays where it is.
                if popup.is_none() && sheet_of.is_none() {
                    focus(&mut focused, Some(window));
                }
            }
            ToRender::CloseWindow { window } => {
                windows.remove(&window);
                with_ca(|ca| ca.drop_window(window));
                if focused == Some(window) {
                    focus(&mut focused, None);
                }
                note(Seen::Closed { window });
            }
            ToRender::Present { window } => {
                if windows.contains_key(&window) {
                    composite(window, None, &mut glyphs);
                    send(FromRender::Frame { window });
                }
            }
            ToRender::Commit(commit) => {
                // Drawn at once, as a compositor that is always ready for a
                // frame would have them.
                with_ca(|ca| ca.apply(*commit));
                for window in windows.keys() {
                    composite(*window, None, &mut glyphs);
                }
            }
            ToRender::Composite { window, at } => composite(window, at, &mut glyphs),
            ToRender::FrameTicks { window, on } => note(Seen::FrameTicks { window, on }),
            ToRender::Request { window, request } => {
                note(Seen::Request { window, request: format!("{request:?}") });
                let Some(win) = windows.get_mut(&window) else { continue };
                match request {
                    WindowRequest::Activate => focus(&mut focused, Some(window)),
                    WindowRequest::Resize(w, h) => {
                        (win.width, win.height) = (w.max(1), h.max(1));
                        if let Some(all) = PIXELS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                            all.insert(
                                window,
                                (win.width, win.height, vec![0; win.width as usize * win.height as usize]),
                            );
                        }
                        send(configure(window, win));
                    }
                    WindowRequest::Maximize(on) => {
                        win.state.maximized = on;
                        send(configure(window, win));
                    }
                    WindowRequest::Fullscreen(on) => {
                        win.state.fullscreen = on;
                        send(configure(window, win));
                    }
                    WindowRequest::Minimize | WindowRequest::Move => {}
                }
            }
            ToRender::SetCursor { window, cursor } => note(Seen::Cursor { window, name: cursor.name().into() }),
            ToRender::HideCursor { hidden, until_moved } => note(Seen::CursorHidden { hidden, until_moved }),
            ToRender::SetParent { window, parent } => note(Seen::Parent { window, parent }),
            ToRender::Paint { window: _, target: crate::protocol::Target::Content(layer), rects, ops } => {
                with_ca(|ca| ca.paint_content(layer, &rects, &ops, &mut glyphs));
            }
            ToRender::Paint { window, target, rects, ops } => {
                let ops = with_ca(|ca| ca.paint(window, target, &rects, ops));
                if target == crate::protocol::Target::Root {
                    capture(window, &rects, &ops, &mut glyphs);
                }
                if let Some(text) = TEXT.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    let runs = ops.iter().filter_map(|op| match op {
                        Op::Glyphs(run) => Some(PaintedText { window, color: run.color, x: run.x, y: run.y }),
                        _ => None,
                    });
                    text.extend(runs);
                }
            }
            ToRender::MenuBar { window, height, .. } => {
                if let Some(win) = windows.get_mut(&window).filter(|w| w.bar != height) {
                    win.bar = height;
                    send(configure(window, win));
                }
            }
            _ => {}
        }
        HANDLED.fetch_add(1, Ordering::Release);
    }
}

fn note(seen: Seen) {
    LOG.lock().unwrap_or_else(|e| e.into_inner()).push(seen);
}
