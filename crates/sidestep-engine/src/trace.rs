//! `SIDESTEP_TRACE_FRAMES`: frame timing on stderr, from both threads.

use std::time::Instant;

/// Whether `SIDESTEP_TRACE_FRAMES` asks for counters (read once).
pub fn tracing() -> bool {
    trace_level() > 0
}

/// Whether `SIDESTEP_TRACE_FRAMES=tiles` asks for more: each tile paint
/// (where, why, how much it drew), each tile a present finishes, and each
/// text view damaging itself after laying out.
pub fn tracing_tiles() -> bool {
    trace_level() > 1
}

fn trace_level() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| match std::env::var("SIDESTEP_TRACE_FRAMES").as_deref() {
        Err(_) | Ok("0") => 0,
        Ok("tiles") => 2,
        Ok(_) => 1,
    })
}

/// Milliseconds since the first traced line, which every
/// `SIDESTEP_TRACE_FRAMES` line starts with, so the main thread's passes
/// and the render thread's presents line up.
pub fn trace_ms() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}
