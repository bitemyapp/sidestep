//! Video on the render thread (see `crate::video`): each video is a
//! subsurface of its window's main surface, a sibling of the content
//! like the decorations and tiles, cropped and scaled by its viewport.
//!
//! **Frames.** A frame's buffer becomes a compositor buffer the first
//! time it shows (linux-dmabuf `create_immed`, or a wl_shm pool on the
//! producer's memfd) and stays one while the producer keeps the buffer,
//! so a decoder's surfaces, shown in turn, are imported once. A frame is
//! attached, damaged and committed on its own: the subsurface is
//! desynchronized, so frames don't wait for the window. With
//! wp_commit_timing_v1 a frame carries the time it is for; with
//! wp_presentation it hears whether it reached the screen. A frame is
//! kept, holding the decoder's surface, until the compositor releases
//! its buffer.
//!
//! **Placing.** Where the video is, its size and whether it's over or
//! under the content belong to the window's frame (subsurface position
//! and stacking are its parent's state), so a new place is applied at
//! the window's next present, together with the content it goes with:
//! the subsurface is made synchronized, its viewport and regions set and
//! committed, the window committed, and the subsurface desynchronized
//! again. A video takes no pointer input (its input region is empty), so
//! what's over it in the window gets the pointer, and says it's opaque
//! and a video (wp_content_type_v1).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use smithay_client_toolkit::compositor::Region;
use smithay_client_toolkit::dmabuf::{DmabufFeedback, DmabufHandler, DmabufState};
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_buffer::{self, WlBuffer};
use smithay_client_toolkit::reexports::client::protocol::wl_callback::{self, WlCallback};
use smithay_client_toolkit::reexports::client::protocol::wl_shm_pool::{self, WlShmPool};
use smithay_client_toolkit::reexports::client::protocol::wl_subsurface::WlSubsurface;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use smithay_client_toolkit::reexports::protocols::wp::commit_timing::v1::client::{
    wp_commit_timer_v1::{self, WpCommitTimerV1},
    wp_commit_timing_manager_v1::{self, WpCommitTimingManagerV1},
};
use smithay_client_toolkit::reexports::protocols::wp::content_type::v1::client::{
    wp_content_type_manager_v1::{self, WpContentTypeManagerV1},
    wp_content_type_v1::{self, WpContentTypeV1},
};
use smithay_client_toolkit::reexports::protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::{self, ZwpIdleInhibitManagerV1},
    zwp_idle_inhibitor_v1::{self, ZwpIdleInhibitorV1},
};
use smithay_client_toolkit::reexports::protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1,
};
use smithay_client_toolkit::reexports::protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::shm::Shm;

use super::{Shell, State};
use crate::protocol::WindowId;
use crate::video::{Shown, VideoBuffer, VideoCaps, VideoFrame, VideoId, VideoPlace};

/// CLOCK_MONOTONIC, as wp_presentation names clocks.
const CLOCK_MONOTONIC: u32 = 1;

/// The render thread's videos, the compositor buffers made for frames,
/// and the globals they use.
pub struct Videos {
    surfaces: HashMap<(WindowId, VideoId), VideoSurface>,
    buffers: HashMap<usize, Cached>,
    dmabuf: DmabufState,
    presentation: Option<WpPresentation>,
    timing: Option<WpCommitTimingManagerV1>,
    content_type: Option<WpContentTypeManagerV1>,
    idle: Option<ZwpIdleInhibitManagerV1>,
    caps: VideoCaps,
    /// Frames shown since the cache of buffers was last swept.
    since_sweep: u32,
}

/// A compositor buffer made for a frame's buffer, kept while the
/// producer keeps the buffer, and the frames shown with it that the
/// compositor may still be reading.
struct Cached {
    buffer: Weak<VideoBuffer>,
    wl: WlBuffer,
    pool: Option<WlShmPool>,
    in_flight: Vec<VideoFrame>,
}

impl Drop for Cached {
    fn drop(&mut self) {
        self.wl.destroy();
        if let Some(p) = &self.pool {
            p.destroy();
        }
    }
}

struct VideoSurface {
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    timer: Option<WpCommitTimerV1>,
    content_type: Option<WpContentTypeV1>,
    inhibitor: Option<ZwpIdleInhibitorV1>,
    /// Where it was asked to be.
    place: VideoPlace,
    /// Where it is, as last applied: x, y, width and height in the
    /// parent's points, and under the content.
    applied: Option<(i32, i32, i32, i32, bool)>,
    /// The crop last given the viewport.
    crop: Option<[f32; 4]>,
    /// The subsurface is synchronized (new, or being placed).
    synced: bool,
}

impl Drop for VideoSurface {
    fn drop(&mut self) {
        if let Some(i) = self.inhibitor.take() {
            i.destroy();
        }
        if let Some(c) = self.content_type.take() {
            c.destroy();
        }
        if let Some(t) = self.timer.take() {
            t.destroy();
        }
        self.viewport.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

/// Where a frame hears whether it reached the screen.
type Report = Box<dyn FnOnce(Shown) + Send>;

/// What a presentation feedback reports to: the frame's `shown`.
pub struct Feedback(Mutex<Option<Report>>);

impl Feedback {
    fn report(&self, shown: Shown) {
        if let Some(f) = self.0.lock().unwrap_or_else(|e| e.into_inner()).take() {
            f(shown);
        }
    }
}

/// A wl_shm pool or buffer made for a frame's buffer.
pub struct ShmTag;

/// The callback after the globals' first events: what the compositor
/// takes is known then.
pub struct CapsKnown;

impl Videos {
    pub fn new(globals: &GlobalList, qh: &QueueHandle<State>) -> Videos {
        let dmabuf = DmabufState::new(globals, qh);
        // Version 4 tells formats through feedback, asked for once.
        if dmabuf.version().is_some_and(|v| v >= 4) {
            let _ = dmabuf.get_default_feedback(qh);
        }
        let presentation = globals.bind::<WpPresentation, _, _>(qh, 1..=2, ()).ok();
        let timing = globals.bind::<WpCommitTimingManagerV1, _, _>(qh, 1..=1, ()).ok();
        let content_type = globals.bind::<WpContentTypeManagerV1, _, _>(qh, 1..=1, ()).ok();
        let idle = globals.bind::<ZwpIdleInhibitManagerV1, _, _>(qh, 1..=1, ()).ok();
        let caps = VideoCaps {
            known: false,
            commit_timing: timing.is_some(),
            presentation: presentation.is_some(),
            ..VideoCaps::default()
        };
        Videos {
            surfaces: HashMap::new(),
            buffers: HashMap::new(),
            dmabuf,
            presentation,
            timing,
            content_type,
            idle,
            caps,
            since_sweep: 0,
        }
    }

    /// Ask for a callback after the events the globals send at once.
    pub fn ask_caps(&self, conn: &Connection, qh: &QueueHandle<State>) {
        conn.display().sync(qh, CapsKnown);
    }

    /// The videos of a window that goes.
    pub fn window_closed(&mut self, window: WindowId) {
        self.surfaces.retain(|(w, _), _| *w != window);
    }
}

impl VideoSurface {
    /// Where its place puts it.
    fn target(&self) -> (i32, i32, i32, i32, bool) {
        let r = self.place.rect.round_out();
        (r.x0 as i32, r.y0 as i32, (r.x1 - r.x0).max(1.0) as i32, (r.y1 - r.y0).max(1.0) as i32, self.place.below)
    }
}

/// Show video `video` of `window` at `place`, or take it away.
pub fn place(state: &mut State, window: WindowId, video: VideoId, place: Option<VideoPlace>) {
    let key = (window, video);
    let Some(place) = place else {
        state.videos.surfaces.remove(&key);
        return;
    };
    if !state.videos.surfaces.contains_key(&key) {
        let Some(win) = state.windows.get(&window) else { return };
        let parent = win.main_surface().clone();
        let (subsurface, surface) = state.subcompositor.create_subsurface(parent, &state.qh);
        let viewport = state.viewporter.get().expect("wp_viewporter").get_viewport(&surface, &state.qh, ());
        let empty = state.empty_region();
        surface.set_input_region(Some(&empty));
        let content_type = state.videos.content_type.as_ref().map(|m| {
            let c = m.get_surface_content_type(&surface, &state.qh, ());
            c.set_content_type(wp_content_type_v1::Type::Video);
            c
        });
        let timer = state.videos.timing.as_ref().map(|m| m.get_timer(&surface, &state.qh, ()));
        state.videos.surfaces.insert(
            key,
            VideoSurface {
                surface,
                subsurface,
                viewport,
                timer,
                content_type,
                inhibitor: None,
                place,
                applied: None,
                crop: None,
                // New subsurfaces are synchronized.
                synced: true,
            },
        );
    }
    let idle = state.videos.idle.clone();
    let qh = state.qh.clone();
    let Some(v) = state.videos.surfaces.get_mut(&key) else { return };
    v.place = place;
    match (&v.inhibitor, place.inhibit_idle, &idle) {
        (None, true, Some(m)) => v.inhibitor = Some(m.create_inhibitor(&v.surface, &qh, ())),
        (Some(i), false, _) => {
            i.destroy();
            v.inhibitor = None;
        }
        _ => {}
    }
}

/// Before the window's commit at a present: put its videos where they
/// were asked to be, to show with what it presents. Returns the
/// subsurfaces to desynchronize after the commit.
pub fn before_commit(state: &mut State, window: WindowId) -> Vec<(WindowId, VideoId)> {
    let Some(win) = state.windows.get(&window) else { return Vec::new() };
    let content = win.surface().clone();
    let top = match &win.shell {
        Shell::Toplevel(_) => win.titlebar() as i32,
        _ => 0,
    };
    let toplevel = matches!(win.shell, Shell::Toplevel(_));
    let mut placed = Vec::new();
    let compositor = &state.compositor;
    for (&key, v) in state.videos.surfaces.iter_mut().filter(|((w, _), _)| *w == window) {
        let target = v.target();
        if v.applied == Some(target) && !v.synced {
            continue;
        }
        let (x, y, w, h, below) = target;
        if !v.synced {
            v.subsurface.set_sync();
            v.synced = true;
        }
        v.subsurface.set_position(x, y + top);
        // A sibling of the content where the content is the frame's
        // subsurface; under or over the content itself otherwise.
        if toplevel {
            if below { v.subsurface.place_below(&content) } else { v.subsurface.place_above(&content) }
        } else if below {
            v.subsurface.place_below(&content);
        }
        v.viewport.set_destination(w, h);
        if let Ok(region) = Region::new(compositor) {
            region.add(0, 0, w, h);
            v.surface.set_opaque_region(Some(region.wl_region()));
        }
        v.surface.commit();
        v.applied = Some(target);
        placed.push(key);
    }
    placed
}

/// After the window's commit: the videos placed go back to showing frames
/// as they come.
pub fn after_commit(state: &mut State, placed: Vec<(WindowId, VideoId)>) {
    for key in placed {
        if let Some(v) = state.videos.surfaces.get_mut(&key) {
            v.subsurface.set_desync();
            v.synced = false;
        }
    }
}

/// Show `frame` in video `video` of `window`.
pub fn show(state: &mut State, window: WindowId, video: VideoId, mut frame: VideoFrame) {
    let key = (window, video);
    if !state.videos.surfaces.contains_key(&key) {
        return; // dropped, as discarded
    }
    let Some(slot) = cached(state, &frame.buffer) else {
        eprintln!("sidestep: a video frame's buffer couldn't be made for the compositor");
        return;
    };
    let qh = state.qh.clone();
    let monotonic = state.videos.caps.monotonic;
    let presentation = state.videos.presentation.clone();
    let Some(entry) = state.videos.buffers.get_mut(&slot) else { return };
    let Some(v) = state.videos.surfaces.get_mut(&key) else { return };
    v.surface.attach(Some(&entry.wl), 0, 0);
    v.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
    if v.crop != Some(frame.crop) {
        let [x, y, w, h] = frame.crop;
        v.viewport.set_source(f64::from(x), f64::from(y), f64::from(w), f64::from(h));
        v.crop = Some(frame.crop);
    }
    if let (Some(at), Some(timer), true) = (frame.at, &v.timer, monotonic) {
        let secs = at / 1_000_000_000;
        timer.set_timestamp((secs >> 32) as u32, secs as u32, (at % 1_000_000_000) as u32);
    }
    match (&presentation, frame.shown.take()) {
        (Some(p), Some(shown)) => {
            p.feedback(&v.surface, &qh, Feedback(Mutex::new(Some(shown))));
        }
        // Without wp_presentation it showed as far as anyone can tell.
        (None, Some(shown)) => shown(Shown::Presented { time: 0, refresh: 0 }),
        _ => {}
    }
    v.surface.commit();
    entry.in_flight.push(frame);
    state.videos.since_sweep += 1;
    if state.videos.since_sweep >= 64 {
        sweep(state);
    }
}

/// Show nothing in a video until its next frame.
pub fn clear(state: &mut State, window: WindowId, video: VideoId) {
    if let Some(v) = state.videos.surfaces.get_mut(&(window, video)) {
        v.surface.attach(None, 0, 0);
        v.surface.commit();
    }
}

/// The cache key of a frame's buffer, made for the compositor if it isn't
/// yet.
fn cached(state: &mut State, buffer: &Arc<VideoBuffer>) -> Option<usize> {
    let key = Arc::as_ptr(buffer) as usize;
    if let Some(c) = state.videos.buffers.get(&key) {
        if c.buffer.upgrade().is_some_and(|b| Arc::ptr_eq(&b, buffer)) {
            return Some(key);
        }
        // Another buffer at a freed one's address: the freed one's frames
        // are all released (they kept it), so its compositor buffer goes.
        state.videos.buffers.remove(&key);
    }
    let (wl, pool) = match &**buffer {
        VideoBuffer::Dmabuf(d) => {
            let params = state.videos.dmabuf.create_params(&state.qh).ok()?;
            for (i, p) in d.planes.iter().enumerate() {
                use std::os::fd::AsFd;
                params.add(p.fd.as_fd(), i as u32, p.offset, p.stride, d.modifier);
            }
            let (wl, _params) = params.create_immed(
                d.width as i32,
                d.height as i32,
                d.fourcc,
                zwp_linux_buffer_params_v1::Flags::empty(),
                &state.qh,
            );
            (wl, None)
        }
        VideoBuffer::Shm(s) => {
            use std::os::fd::AsFd;
            let shm: &Shm = &state.shm;
            let pool = shm.wl_shm().create_pool(s.fd.as_fd(), s.size as i32, &state.qh, ShmTag);
            let format = WEnum::<smithay_client_toolkit::reexports::client::protocol::wl_shm::Format>::from(s.format);
            let WEnum::Value(format) = format else { return None };
            let wl = pool.create_buffer(
                s.offset as i32,
                s.width as i32,
                s.height as i32,
                s.stride as i32,
                format,
                &state.qh,
                ShmTag,
            );
            (wl, Some(pool))
        }
    };
    state.videos.buffers.insert(key, Cached { buffer: Arc::downgrade(buffer), wl, pool, in_flight: Vec::new() });
    Some(key)
}

/// Destroy the compositor buffers whose frames' buffers the producer let
/// go of and the compositor has released.
fn sweep(state: &mut State) {
    state.videos.since_sweep = 0;
    // Frames in flight keep their buffers.
    state.videos.buffers.retain(|_, c| c.buffer.strong_count() > 0);
}

/// The compositor is done reading a buffer: its frames go, and with them
/// what they kept.
fn released(state: &mut State, wl: &WlBuffer) {
    let id = wl.id();
    let mut gone = None;
    for (k, c) in &mut state.videos.buffers {
        if c.wl.id() == id {
            c.in_flight.clear();
            if c.buffer.strong_count() == 0 {
                gone = Some(*k);
            }
            break;
        }
    }
    if let Some(k) = gone {
        state.videos.buffers.remove(&k);
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.videos.dmabuf
    }

    fn dmabuf_feedback(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        feedback: DmabufFeedback,
    ) {
        let table = feedback.format_table();
        let mut formats: Vec<(u32, u64)> = feedback
            .tranches()
            .iter()
            .flat_map(|t| t.formats.iter())
            .filter_map(|&i| table.get(usize::from(i)))
            .map(|f| (f.format, f.modifier))
            .collect();
        formats.sort_unstable();
        formats.dedup();
        self.videos.caps.dmabuf = formats;
        self.videos.caps.main_device = feedback.main_device();
        proxy.destroy();
        if self.videos.caps.known {
            crate::video::publish_caps(self.videos.caps.clone());
        }
    }

    fn created(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        _: WlBuffer,
    ) {
    }

    fn failed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
    ) {
    }

    fn released(&mut self, _: &Connection, _: &QueueHandle<Self>, buffer: &WlBuffer) {
        released(self, buffer);
    }
}

smithay_client_toolkit::delegate_dmabuf!(State);

impl Dispatch<WlCallback, CapsKnown> for State {
    fn event(
        state: &mut State,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &CapsKnown,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let wl_callback::Event::Done { .. } = event else { return };
        let v = &mut state.videos;
        if v.dmabuf.version().is_some_and(|ver| ver < 4) {
            let mut formats: Vec<(u32, u64)> = v.dmabuf.modifiers().iter().map(|f| (f.format, f.modifier)).collect();
            formats.sort_unstable();
            formats.dedup();
            v.caps.dmabuf = formats;
        }
        v.caps.shm = state.shm.formats().iter().map(|f| u32::from(*f)).collect();
        v.caps.known = true;
        crate::video::publish_caps(v.caps.clone());
    }
}

impl Dispatch<WlShmPool, ShmTag> for State {
    fn event(_: &mut State, _: &WlShmPool, _: wl_shm_pool::Event, _: &ShmTag, _: &Connection, _: &QueueHandle<State>) {}
}

impl Dispatch<WlBuffer, ShmTag> for State {
    fn event(
        state: &mut State,
        buffer: &WlBuffer,
        event: wl_buffer::Event,
        _: &ShmTag,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_buffer::Event::Release = event {
            released(state, buffer);
        }
    }
}

impl Dispatch<WpPresentation, ()> for State {
    fn event(
        state: &mut State,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.videos.caps.monotonic = clk_id == CLOCK_MONOTONIC;
        }
    }
}

impl Dispatch<WpPresentationFeedback, Feedback> for State {
    fn event(
        _: &mut State,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        feedback: &Feedback,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented { tv_sec_hi, tv_sec_lo, tv_nsec, refresh, .. } => {
                let secs = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                feedback.report(Shown::Presented { time: secs * 1_000_000_000 + u64::from(tv_nsec), refresh });
            }
            wp_presentation_feedback::Event::Discarded => feedback.report(Shown::Discarded),
            _ => {}
        }
    }
}

impl Dispatch<WpCommitTimingManagerV1, ()> for State {
    fn event(
        _: &mut State,
        _: &WpCommitTimingManagerV1,
        _: wp_commit_timing_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WpCommitTimerV1, ()> for State {
    fn event(
        _: &mut State,
        _: &WpCommitTimerV1,
        _: wp_commit_timer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WpContentTypeManagerV1, ()> for State {
    fn event(
        _: &mut State,
        _: &WpContentTypeManagerV1,
        _: wp_content_type_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WpContentTypeV1, ()> for State {
    fn event(
        _: &mut State,
        _: &WpContentTypeV1,
        _: wp_content_type_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwpIdleInhibitManagerV1, ()> for State {
    fn event(
        _: &mut State,
        _: &ZwpIdleInhibitManagerV1,
        _: zwp_idle_inhibit_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwpIdleInhibitorV1, ()> for State {
    fn event(
        _: &mut State,
        _: &ZwpIdleInhibitorV1,
        _: zwp_idle_inhibitor_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}
