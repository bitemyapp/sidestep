//! Driving the toolkit without a display, for tests (not a stable API).
//!
//! [`use_null_backend`] (or `SIDESTEP_BACKEND=null`) replaces the Wayland
//! render thread with one that answers at once, as a compositor that is
//! always ready would: a window is configured at the size it asked for,
//! at scale 1, and a toplevel gets the keyboard; every present is shown
//! at once. It draws nothing unless asked to keep windows' pixels
//! ([`capture_pixels`], then [`window_pixels`]). Input comes only from
//! tests: each `inject_…` function queues a message as if the render
//! thread had sent it, after whatever it did send, for the loop's next
//! turn.

use kurbo::{Point, Vec2};
use sidestep_engine::backend::null;
use sidestep_engine::keys::modifier;
use sidestep_engine::protocol::{self, Button, FromRender, ScrollPhase};

pub use sidestep_engine::backend::null::{PaintedText, Seen};

use crate::app::Cx;
use crate::event::{Modifiers, PointerButton};
use crate::window::WindowId;

/// Use the null render thread. Call before [`App::run`](crate::App::run).
pub fn use_null_backend() {
    null::request();
}

/// Keep (or stop keeping) every window's pixels: what its paints drew,
/// at scale 1. Call before the windows open.
pub fn capture_pixels(on: bool) {
    *null::PIXELS.lock().unwrap_or_else(|e| e.into_inner()) = on.then(Default::default);
}

/// A window's captured pixels: width, height and premultiplied RGBA, row
/// by row from the top left. The render thread draws a paint after the
/// loop sends it, so look once the window has shown the frame (the next
/// turn after drawing, or later).
pub fn window_pixels(window: WindowId) -> Option<(u32, u32, Vec<[u8; 4]>)> {
    let all = null::PIXELS.lock().unwrap_or_else(|e| e.into_inner());
    let (w, h, px) = all.as_ref()?.get(&window.0)?;
    Some((*w, *h, px.iter().map(|p| p.to_ne_bytes()).collect()))
}

/// Start (or stop) writing down the text painted, for
/// [`take_painted_text`].
pub fn note_painted_text(on: bool) {
    *null::TEXT.lock().unwrap_or_else(|e| e.into_inner()) = on.then(Vec::new);
}

/// The text painted since the last call, while noting it.
pub fn take_painted_text() -> Vec<PaintedText> {
    null::TEXT.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(std::mem::take).unwrap_or_default()
}

/// What the loop asked of the render thread besides drawing, since the
/// last call.
pub fn take_render_log() -> Vec<Seen> {
    std::mem::take(&mut *null::LOG.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Whether the render thread has handled everything sent to it.
pub fn render_idle() -> bool {
    use std::sync::atomic::Ordering;
    null::HANDLED.load(Ordering::Acquire) >= null::SENT.load(Ordering::Acquire)
}

fn flags(m: Modifiers) -> protocol::Modifiers {
    let mut f = 0;
    for (ours, bit) in [
        (Modifiers::SHIFT, modifier::SHIFT),
        (Modifiers::CONTROL, modifier::CONTROL),
        (Modifiers::ALT, modifier::OPTION),
        (Modifiers::SUPER, modifier::COMMAND),
        (Modifiers::CAPS_LOCK, modifier::CAPS_LOCK),
    ] {
        if m.contains(ours) {
            f |= bit;
        }
    }
    f
}

/// A key typing `characters` (what it types with `modifiers`) and
/// `unmodified` (with Shift alone), pressed or released, with evdev code
/// `code`.
pub fn inject_key(
    cx: &mut Cx,
    window: WindowId,
    code: u32,
    characters: &str,
    unmodified: &str,
    modifiers: Modifiers,
    pressed: bool,
) {
    let key = protocol::Key {
        down: pressed,
        repeat: false,
        code: (code + 8) as u16,
        characters: characters.into(),
        unmodified: unmodified.into(),
        modifiers: flags(modifiers),
        composing: None,
    };
    cx.inject(FromRender::Key { window: window.0, key });
}

/// The pointer moving to `at`.
pub fn inject_motion(cx: &mut Cx, window: WindowId, at: Point) {
    cx.inject(FromRender::Motion { window: window.0, x: at.x, y: at.y, modifiers: 0 });
}

/// A button pressed or released at `at`, as click `clicks` of a series.
pub fn inject_button(cx: &mut Cx, window: WindowId, at: Point, button: PointerButton, pressed: bool, clicks: u32) {
    let button = match button {
        PointerButton::Left => Button::Left,
        PointerButton::Right => Button::Right,
        PointerButton::Middle => Button::Other(2),
        PointerButton::Back => Button::Other(3),
        PointerButton::Forward => Button::Other(4),
        PointerButton::Other(n) => Button::Other(n),
    };
    cx.inject(FromRender::Button {
        window: window.0,
        x: at.x,
        y: at.y,
        button,
        pressed,
        clicks,
        modifiers: 0,
        activating: false,
    });
}

/// A wheel turned by `lines` at `at`.
pub fn inject_wheel(cx: &mut Cx, window: WindowId, at: Point, lines: Vec2) {
    cx.inject(FromRender::Scroll {
        window: window.0,
        x: at.x,
        y: at.y,
        dx: lines.x,
        dy: lines.y,
        wheel: true,
        modifiers: 0,
        phase: ScrollPhase::None,
        velocity: (0.0, 0.0),
        inverted: false,
    });
}

/// The user asking to close the window (its close button).
pub fn inject_close_request(cx: &mut Cx, window: WindowId) {
    cx.inject(FromRender::CloseRequested { window: window.0 });
}

/// An input method committing `text`, then composing `preedit`.
pub fn inject_ime(cx: &mut Cx, window: WindowId, commit: Option<&str>, preedit: &str) {
    let end = preedit.len() as i32;
    cx.inject(FromRender::TextInput {
        window: window.0,
        commit: commit.map(str::to_owned),
        preedit: (preedit.to_owned(), end, end),
    });
}

/// Drag `drag` entering the window at `at`, offering `mimes` and, as
/// `text/uri-list`, `urls`; the source allows copying.
pub fn inject_drag_enter(cx: &mut Cx, drag: u64, window: WindowId, at: Point, mimes: &[&str], urls: &[&str]) {
    let list: String = urls.iter().map(|u| format!("{u}\r\n")).collect();
    let urls = (!urls.is_empty()).then(|| ("text/uri-list".to_owned(), std::sync::Arc::from(list.into_bytes())));
    cx.inject(FromRender::DndEnter {
        drag,
        window: window.0,
        x: at.x,
        y: at.y,
        mimes: mimes.iter().map(|m| (*m).to_owned()).collect(),
        actions: 1,
        urls,
    });
}

/// Drag `drag` dropped where it is.
pub fn inject_drop(cx: &mut Cx, drag: u64) {
    cx.inject(FromRender::DndDrop { drag });
}

/// The id the render thread knows `window` by (in [`Seen`] and
/// [`PaintedText`]).
pub fn render_id(window: WindowId) -> u32 {
    window.0
}

/// The compositor dismissing a popup (a click elsewhere).
pub fn inject_popup_done(cx: &mut Cx, popup: WindowId) {
    cx.inject(FromRender::PopupDone { window: popup.0 });
}
