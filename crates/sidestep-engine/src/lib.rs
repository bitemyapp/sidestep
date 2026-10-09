//! Sidestep's engine: what draws and talks to the display server, shared
//! by the toolkits built on it — `sidestep-appkit` (AppKit, for objc2
//! programs) and `sidestep-ui` (a native Rust API). Nothing here touches
//! the Objective-C runtime.
//!
//! Two threads share the work, and [`protocol`] is what they tell each
//! other. The toolkit's main thread runs the program: events, timers,
//! layout and drawing, which records [`protocol::Op`]s rather than touching
//! pixels. The render thread ([`backend`]) owns the Wayland connection,
//! rasterizes those ops into per-layer caches ([`raster`]), composites
//! Core Animation's layer trees ([`ca`]), presents, and sends input and
//! frame timing back. Text is shaped and laid out on the thread that asks
//! ([`text`]); the render thread only rasterizes glyphs.
//!
//! Beside them: the clipboard's shared state ([`clipboard`]), the
//! compositor's outputs ([`outputs`]), the desktop's settings and system
//! colors ([`settings`], [`palette`]), image files ([`codec`]), paths
//! ([`path`]) and `SIDESTEP_TRACE_FRAMES` ([`trace`]).
//!
//! This crate's API serves the toolkits and changes with them; programs
//! use a toolkit.
#![cfg(not(target_vendor = "apple"))]

pub mod backend;
pub mod ca;
pub mod clipboard;
pub mod codec;
pub mod color;
pub mod desktop;
pub mod keys;
pub mod outputs;
pub mod palette;
pub mod path;
pub mod protocol;
pub mod raster;
pub mod settings;
pub mod text;
pub mod trace;
