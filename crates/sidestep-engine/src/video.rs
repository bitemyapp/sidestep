//! Video in a window: frames a decoder made, shown as they are.
//!
//! A video is a subsurface of its window (see `backend::video`), placed
//! in the content in whole points and scaled by the compositor
//! (wp_viewporter), so the frames are never copied or drawn here: a
//! hardware decoder's dmabufs go to the compositor as they are, which can
//! put them on a display plane of their own, and a software decoder's
//! pixels go once into shared memory. Nothing of the window's own drawing
//! is involved, and a frame costs the render thread an attach and a
//! commit.
//!
//! The thread that has the frames (a player's) shows them through a
//! [`VideoSink`](crate::protocol::ToRender::ShowVideo) straight to the
//! render thread, each with the time it is for: with wp_commit_timing_v1
//! the compositor shows it at the first refresh at or after that time,
//! else as soon as it can. Each frame hears once whether it reached the
//! screen ([`Shown`]), from wp_presentation, which is how a player knows
//! its window is out of sight (frames discarded) and may stop decoding.
//!
//! Over the window's content the compositor can scan the video out
//! without composing it; under it (`VideoPlace::below`), what the content
//! draws over the video (controls, subtitles) shows where the content is
//! see-through.

use std::os::fd::OwnedFd;
use std::sync::Arc;

use crate::protocol::Rect;

/// A video of a window, as the window names it.
pub type VideoId = u32;

/// Where a video shows in its window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoPlace {
    /// In the window's content, in whole points.
    pub rect: Rect,
    /// Under the content's surface, which shows it where it is see-through
    /// (and draws over it elsewhere); else over the content.
    pub below: bool,
    /// Keep the screen from dimming, blanking and locking.
    pub inhibit_idle: bool,
}

impl VideoPlace {
    /// A video at `x`, `y`, `width` × `height` points of the content.
    pub fn new(x: f64, y: f64, width: f64, height: f64, below: bool, inhibit_idle: bool) -> VideoPlace {
        let rect = Rect::new(x as f32, y as f32, (x + width) as f32, (y + height) as f32);
        VideoPlace { rect, below, inhibit_idle }
    }

    /// Where it is: x, y, width and height in points.
    pub fn bounds(&self) -> [f64; 4] {
        let r = &self.rect;
        [f64::from(r.x0), f64::from(r.y0), f64::from(r.x1 - r.x0), f64::from(r.y1 - r.y0)]
    }
}

/// A buffer of memory frames show: the same memory shown again is the same
/// buffer (the same `Arc`), which the render thread keeps its compositor
/// buffer for while the producer keeps the `Arc`.
#[derive(Debug)]
pub enum VideoBuffer {
    Dmabuf(Dmabuf),
    Shm(ShmBuffer),
}

impl VideoBuffer {
    /// Its size in pixels.
    pub fn size(&self) -> (u32, u32) {
        match self {
            VideoBuffer::Dmabuf(d) => (d.width, d.height),
            VideoBuffer::Shm(s) => (s.width, s.height),
        }
    }
}

/// A linux-dmabuf buffer: its planes' memory, in a DRM format and layout.
#[derive(Debug)]
pub struct Dmabuf {
    pub width: u32,
    pub height: u32,
    /// The DRM fourcc (`NV12`, `P010`, …).
    pub fourcc: u32,
    /// The DRM format modifier of every plane.
    pub modifier: u64,
    pub planes: Vec<DmabufPlane>,
}

#[derive(Debug)]
pub struct DmabufPlane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

/// Pixels in shared memory (a memfd the producer writes), in a wl_shm
/// format.
#[derive(Debug)]
pub struct ShmBuffer {
    pub fd: OwnedFd,
    /// The file's size, all of which the compositor maps.
    pub size: usize,
    pub offset: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// The wl_shm format code (XRGB8888 is 1).
    pub format: u32,
}

/// What became of a frame shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shown {
    /// On the screen at `time` (nanoseconds of the presentation clock,
    /// CLOCK_MONOTONIC), the display refreshing every `refresh`
    /// nanoseconds (0 if it doesn't say).
    Presented { time: u64, refresh: u32 },
    /// Never reached the screen: replaced first, or its window out of
    /// sight.
    Discarded,
}

/// A frame to show.
pub struct VideoFrame {
    pub buffer: Arc<VideoBuffer>,
    /// The part of the buffer that is the picture, in its pixels: x, y,
    /// width, height (a decoder's surfaces are often taller than the
    /// picture).
    pub crop: [f32; 4],
    /// When it should show, in nanoseconds of CLOCK_MONOTONIC; none for
    /// as soon as it can.
    pub at: Option<u64>,
    /// What the compositor did with it: called once.
    pub shown: Option<Box<dyn FnOnce(Shown) + Send>>,
    /// Kept until the compositor is done reading the buffer (dropped on
    /// its release): what keeps a decoder from writing into it meanwhile.
    pub keep: Option<Box<dyn std::any::Any + Send>>,
}

impl VideoFrame {
    pub fn new(buffer: Arc<VideoBuffer>) -> VideoFrame {
        let (w, h) = buffer.size();
        VideoFrame { buffer, crop: [0.0, 0.0, w as f32, h as f32], at: None, shown: None, keep: None }
    }

    /// Say what became of it, once.
    pub fn report(&mut self, shown: Shown) {
        if let Some(f) = self.shown.take() {
            f(shown);
        }
    }
}

impl Drop for VideoFrame {
    fn drop(&mut self) {
        // A frame dropped unshown (its window gone, or never placed).
        self.report(Shown::Discarded);
    }
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("buffer", &self.buffer)
            .field("crop", &self.crop)
            .field("at", &self.at)
            .finish()
    }
}

/// What the compositor takes, known once the render thread has asked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoCaps {
    /// The render thread has heard from the compositor.
    pub known: bool,
    /// linux-dmabuf formats and modifiers it imports (none without the
    /// protocol: a compositor rendering in software).
    pub dmabuf: Vec<(u32, u64)>,
    /// wl_shm formats.
    pub shm: Vec<u32>,
    /// wp_commit_timing_v1: frames show at the time they ask for.
    pub commit_timing: bool,
    /// wp_presentation: frames hear when they showed.
    pub presentation: bool,
    /// The presentation clock is CLOCK_MONOTONIC (the clock frames' times
    /// count in); else their times are ignored.
    pub monotonic: bool,
    /// The device the compositor renders with (a dev_t; 0 if it doesn't
    /// say): a decoder there makes buffers it imports best.
    pub main_device: u64,
}

impl VideoCaps {
    /// Whether dmabufs of `fourcc` with `modifier` can be shown.
    pub fn takes_dmabuf(&self, fourcc: u32, modifier: u64) -> bool {
        self.dmabuf.iter().any(|&(f, m)| f == fourcc && m == modifier)
    }
}

/// A render thread has started (and will publish what it takes).
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A render thread starts: `caps` waits for what it takes.
pub fn render_started() {
    STARTED.store(true, std::sync::atomic::Ordering::Release);
}

static CAPS: std::sync::RwLock<Option<VideoCaps>> = std::sync::RwLock::new(None);
static CAPS_KNOWN: std::sync::Condvar = std::sync::Condvar::new();
static CAPS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What the running render thread's compositor takes for video, waiting up
/// to `wait` for it to have asked; none if it hasn't (no window shown yet,
/// or no display).
pub fn caps(wait: std::time::Duration) -> Option<VideoCaps> {
    if !STARTED.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    let deadline = std::time::Instant::now() + wait;
    let mut guard = CAPS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(c) = CAPS.read().unwrap_or_else(|e| e.into_inner()).clone().filter(|c| c.known) {
            return Some(c);
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return None;
        }
        guard = CAPS_KNOWN.wait_timeout(guard, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// Publish what the compositor takes (the render thread, once it knows).
pub fn publish_caps(caps: VideoCaps) {
    let _guard = CAPS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    *CAPS.write().unwrap_or_else(|e| e.into_inner()) = Some(caps);
    CAPS_KNOWN.notify_all();
}

/// Now on CLOCK_MONOTONIC, in nanoseconds: the clock frames are timed by.
pub fn monotonic_now() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// DRM fourcc codes.
pub const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

pub const NV12: u32 = fourcc(b"NV12");
pub const P010: u32 = fourcc(b"P010");
pub const XRGB8888: u32 = fourcc(b"XR24");
/// DRM_FORMAT_MOD_LINEAR.
pub const MOD_LINEAR: u64 = 0;
/// DRM_FORMAT_MOD_INVALID: the implicit modifier.
pub const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourccs_are_drms() {
        assert_eq!(NV12, 0x3231_564e);
        assert_eq!(P010, 0x3031_3050);
        assert_eq!(XRGB8888, 0x3432_5258);
    }

    #[test]
    fn a_frame_dropped_unshown_says_so_once() {
        let fd: OwnedFd = std::fs::File::open("/dev/null").expect("/dev/null").into();
        let buffer =
            Arc::new(VideoBuffer::Shm(ShmBuffer { fd, size: 4, offset: 0, width: 1, height: 1, stride: 4, format: 1 }));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut frame = VideoFrame::new(buffer);
        frame.shown = Some(Box::new(move |s| tx.send(s).expect("receiver")));
        assert_eq!(frame.crop, [0.0, 0.0, 1.0, 1.0]);
        drop(frame);
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![Shown::Discarded]);
    }
}
