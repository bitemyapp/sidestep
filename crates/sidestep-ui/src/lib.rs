//! A native Rust UI toolkit for Linux, on Sidestep's engine.
//!
//! The same machinery that runs Sidestep's AppKit — a render thread that
//! owns the Wayland connection and rasterizes on the CPU, a text engine
//! with fontconfig's fonts, shaping, bidi and color emoji, keyboards
//! through XKB keymaps and compose tables, input methods, the clipboard,
//! drag and drop, client-side decorations where the desktop wants them,
//! the desktop's light or dark appearance and accent color — with a Rust
//! API and none of AppKit: no Objective-C runtime, no message sends, no
//! class hierarchy. A program is a [`Handler`] that [`App::run`] calls
//! with what happens; it opens windows, draws them on a [`Canvas`] when
//! they need it, and lays text out with [`TextLayout`].
//!
// On macOS the crate is empty, so the example isn't built there.
#![cfg_attr(not(target_vendor = "apple"), doc = "```no_run")]
#![cfg_attr(target_vendor = "apple", doc = "```ignore")]
//! use sidestep_ui::{App, Canvas, Color, Cx, Font, Handler, TextStyle, WindowId, WindowOptions};
//! use sidestep_ui::kurbo::{Point, RoundedRect};
//!
//! struct Hello;
//!
//! impl Handler for Hello {
//!     fn launched(&mut self, cx: &mut Cx) {
//!         cx.open_window(WindowOptions::new("Hello").size(400.0, 200.0));
//!     }
//!
//!     fn draw(&mut self, _cx: &mut Cx, _window: WindowId, canvas: &mut Canvas) {
//!         let accent = canvas.system_color(sidestep_ui::SystemColor::Accent);
//!         canvas.fill(&RoundedRect::new(20.0, 20.0, 380.0, 180.0, 12.0), accent);
//!         let style = TextStyle::new(Font::system(24.0).bold(), Color::WHITE);
//!         canvas.draw_label("Hello from Sidestep", &style, Point::new(40.0, 80.0));
//!     }
//! }
//!
//! fn main() {
//!     App::new().run(Hello).expect("a Wayland display");
//! }
//! ```
//!
//! **Threads.** The handler runs on the thread that called [`App::run`],
//! and so does drawing, which records operations rather than touching
//! pixels; the render thread rasterizes them into the parts of each
//! window that changed, at the window's scale, and presents in step with
//! the compositor's frame callbacks. A window draws when it was
//! invalidated ([`Window::invalidate`], [`Window::redraw`]), resized or
//! first shown, and only once its last frame showed. Work on other threads
//! wakes the loop with a [`Proxy`].
//!
//! **Coordinates** are points, from a window's content's top left, y down.
//! Geometry is [`kurbo`]'s, re-exported: anything that is a
//! [`kurbo::Shape`] can be filled, stroked or clipped to.
//!
//! **Desktop surfaces.** A desktop environment's own programs open layer
//! surfaces ([`layer_shell`]: panels, wallpapers, notifications) and lock
//! the session with a lock screen on each output ([`Cx::lock_session`],
//! [`WindowOptions::lock`]); [`Cx::outputs`] names the outputs to put
//! them on, [`Cx::request_activation_token`] lets the programs they
//! launch come up with the keyboard, and [`Cx::frame_stats`] (or
//! `SIDESTEP_FRAME_STATS=1`) measures what frames take.
//!
//! **Offscreen.** [`offscreen::render`] draws on a canvas into an
//! [`Image`] on any thread, with no application running and no display,
//! through the same rasterizer and text engine as windows: for a
//! compositor's own labels, and golden-image tests.
//!
//! **Testing.** `SIDESTEP_BACKEND=null` runs a render thread without a
//! display, which configures windows at once at the size they asked for,
//! shows every frame at once and draws nothing (see `tests/headless.rs`);
//! `tests/wayland_system.rs` drives a window under a headless compositor
//! with a virtual pointer and other clients.
//!
//! A process runs one application at a time, with this toolkit or with
//! AppKit; another may run after it.
#![cfg(not(target_vendor = "apple"))]

mod app;
mod canvas;
mod color;
mod event;
mod image;
pub mod layer_shell;
pub mod offscreen;
mod text;
pub mod video;
mod window;

#[doc(hidden)]
pub mod testing;

pub use app::{
    ActivationRequest, App, Cx, Drag, DragId, DragSource, DropAction, DropResponse, Error, FrameStats, FrameTiming,
    Handler, Output, Proxy, TimerId,
};
pub use canvas::{
    BlendMode, Canvas, ImageOptions, LineCap, LineJoin, LinearGradient, Paint, RadialGradient, Shadow, StrokeStyle,
};
pub use color::{Appearance, Color, SystemColor};
pub use event::{
    Ime, Key, KeyEvent, Modifiers, NamedKey, PointerButton, ScrollDelta, ScrollPhase, WindowEvent, WindowState,
};
pub use image::Image;
pub use kurbo;
pub use text::{
    Alignment, Caret, Font, FontMetrics, Hit, LineMetrics, TextAttachment, TextLayout, TextLayoutBuilder, TextStyle,
    Wrap, measure,
};
pub use window::{Background, Corner, Cursor, PopupPosition, Window, WindowId, WindowOptions, gather_damage};
