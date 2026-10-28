//! Video in a window: a player's frames, shown by the compositor as they
//! are, never drawn on a [`Canvas`](crate::Canvas).
//!
//! A window places a video ([`Window::place_video`](crate::Window::place_video))
//! in its content, in whole points, over what it draws or under it; it
//! shows there the frames a [`VideoSink`] is given, from any thread,
//! scaled to the place by the compositor. Over the content the compositor
//! can scan the video out on a display plane of its own, composing
//! nothing; under it, the window draws over the video wherever it leaves
//! its pixels clear (a window with [`Background::Transparent`](crate::Background)),
//! for controls and subtitles. The window's place goes with its next
//! frame; frames go as they come. See `sidestep_engine::video`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use sidestep_engine::protocol::ToRender;
use smithay_client_toolkit::reexports::calloop::channel::Sender;

pub use sidestep_engine::video::{
    Dmabuf, DmabufPlane, MOD_INVALID, MOD_LINEAR, NV12, P010, ShmBuffer, Shown, VideoBuffer, VideoCaps, VideoFrame,
    VideoId, VideoPlace, XRGB8888, fourcc, monotonic_now,
};

/// What the compositor takes for video, once the toolkit has heard (it
/// asks as it starts): waits up to `wait`. None where no display was
/// reached.
pub fn caps(wait: std::time::Duration) -> Option<VideoCaps> {
    sidestep_engine::video::caps(wait)
}

/// Where a video's frames go: to the render thread, for its window's
/// current showing (nowhere while the window is hidden, where frames are
/// discarded), or, detached, to no window at all.
#[derive(Clone)]
pub struct VideoSink {
    to: To,
    video: VideoId,
}

#[derive(Clone)]
enum To {
    Window { tx: Sender<ToRender>, showing: Arc<AtomicU32> },
    Detached(Arc<Detached>),
}

/// A detached sink's account of what it was given, for tests.
#[derive(Default)]
pub struct Detached {
    shown: AtomicU64,
    hidden: AtomicBool,
    cleared: AtomicU64,
    last_size: AtomicU64,
}

impl Detached {
    /// Frames shown (presented).
    pub fn shown(&self) -> u64 {
        self.shown.load(Ordering::Relaxed)
    }

    /// Times it was cleared.
    pub fn cleared(&self) -> u64 {
        self.cleared.load(Ordering::Relaxed)
    }

    /// The size of the last frame shown, in pixels.
    pub fn last_size(&self) -> Option<(u32, u32)> {
        let v = self.last_size.load(Ordering::Relaxed);
        (v != 0).then_some(((v >> 32) as u32, v as u32))
    }

    /// Play a window out of sight: frames are discarded until it's back.
    pub fn set_hidden(&self, hidden: bool) {
        self.hidden.store(hidden, Ordering::Relaxed);
    }
}

impl VideoSink {
    pub(crate) fn new(tx: Sender<ToRender>, showing: Arc<AtomicU32>, video: VideoId) -> VideoSink {
        VideoSink { to: To::Window { tx, showing }, video }
    }

    /// A sink for no window: every frame is presented at once (or, while
    /// [`Detached::set_hidden`], discarded), and released. For tests and
    /// for playing with nothing to show.
    pub fn detached() -> (VideoSink, Arc<Detached>) {
        let d = Arc::new(Detached::default());
        (VideoSink { to: To::Detached(d.clone()), video: 0 }, d)
    }

    /// Show `frame` as soon as it can, or at its time (`VideoFrame::at`).
    /// It hears what became of it; while the window is hidden, it's
    /// discarded.
    pub fn show(&self, mut frame: VideoFrame) {
        match &self.to {
            To::Window { tx, showing } => {
                let window = showing.load(Ordering::Acquire);
                if window == 0 {
                    return; // dropped: discarded
                }
                sidestep_engine::backend::null::sending();
                let _ = tx.send(ToRender::ShowVideo { window, video: self.video, frame: Box::new(frame) });
            }
            To::Detached(d) => {
                if d.hidden.load(Ordering::Relaxed) {
                    frame.report(Shown::Discarded);
                    return;
                }
                let (w, h) = frame.buffer.size();
                d.last_size.store((u64::from(w) << 32) | u64::from(h), Ordering::Relaxed);
                d.shown.fetch_add(1, Ordering::Relaxed);
                frame.report(Shown::Presented { time: monotonic_now(), refresh: 16_666_667 });
            }
        }
    }

    /// Show nothing until the next frame.
    pub fn clear(&self) {
        match &self.to {
            To::Window { tx, showing } => {
                let window = showing.load(Ordering::Acquire);
                if window != 0 {
                    sidestep_engine::backend::null::sending();
                    let _ = tx.send(ToRender::ClearVideo { window, video: self.video });
                }
            }
            To::Detached(d) => {
                d.cleared.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl std::fmt::Debug for VideoSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let to = match &self.to {
            To::Window { showing, .. } => format!("window showing {}", showing.load(Ordering::Relaxed)),
            To::Detached(_) => "detached".into(),
        };
        f.debug_struct("VideoSink").field("to", &to).field("video", &self.video).finish()
    }
}
