//! Frame statistics: how long each window's frames take of the CPU, for
//! a desktop that holds its frames to a budget.
//!
//! `SIDESTEP_FRAME_STATS=1` collects them and prints a summary on stderr
//! every five seconds, a line for each window that showed frames: how
//! many, and the median, 95th percentile and slowest of each part of a
//! frame in microseconds (recording its drawing on the main thread,
//! rasterizing it into a buffer on the render thread, presenting it:
//! catching the buffer up with the last and committing), of the whole,
//! and how much of the window it redrew. A toolkit may collect them
//! without the printing ([`set_period`]) and read the latest summaries
//! ([`latest`]).
//!
//! A frame is what a main thread's `Present` shows: the toolkit notes how
//! long it took to record ([`recorded`], before sending the paint), and
//! the render thread, presenting it, pairs that with what it measured
//! ([`frame`]). Frames the render thread draws on its own (layer trees
//! animating) aren't counted. Off, each of these costs a relaxed atomic
//! load.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How often summaries are made unless a toolkit says otherwise.
pub const PERIOD: Duration = Duration::from_secs(5);

/// 0 until first asked, then 1 off or 2 on.
static STATE: AtomicU8 = AtomicU8::new(0);

/// Whether frames are measured.
#[inline]
pub fn on() -> bool {
    match STATE.load(Ordering::Relaxed) {
        2 => true,
        1 => false,
        _ => {
            let on = printing() || store().period.is_some();
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

/// Whether `SIDESTEP_FRAME_STATS` asks for summaries on stderr (read once).
pub fn printing() -> bool {
    static PRINTING: OnceLock<bool> = OnceLock::new();
    *PRINTING.get_or_init(|| std::env::var("SIDESTEP_FRAME_STATS").is_ok_and(|v| !v.is_empty() && v != "0"))
}

/// Collect frames, summarizing them every `period` from now on, the frames
/// and summaries so far dropped (none: stop, unless `SIDESTEP_FRAME_STATS`
/// asks for them).
pub fn set_period(period: Option<Duration>) {
    let mut s = store();
    s.period = period.map(|p| p.max(Duration::from_millis(1)));
    let on = printing() || s.period.is_some();
    if !on {
        s.recorded.clear();
    }
    s.frames.clear();
    s.latest.clear();
    s.since = None;
    STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
}

/// Percentiles of a part of the frames, in microseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timing {
    pub p50: u32,
    pub p95: u32,
    pub max: u32,
}

/// A window's frames over a period.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub window: u32,
    pub frames: u32,
    /// Recording on the main thread (none for a toolkit that doesn't
    /// say: AppKit).
    pub record: Option<Timing>,
    pub rasterize: Timing,
    pub present: Timing,
    /// The three together, frame by frame.
    pub total: Timing,
    /// How much of the window a frame redrew, on average: 0 to 1.
    pub damage: f32,
    /// The period summarized.
    pub period: Duration,
}

/// The latest summaries, a window each, oldest window first.
pub fn latest() -> Vec<Summary> {
    store().latest.clone()
}

/// A toolkit recorded a frame of render-thread window `window` in `took`
/// and is about to send it.
pub fn recorded(window: u32, took: Duration) {
    if !on() {
        return;
    }
    let mut s = store();
    let waiting = s.recorded.entry(window).or_default();
    // A frame the render thread never presents (its window went) leaves
    // its recording here: only the latest few wait.
    if waiting.len() >= 4 {
        waiting.pop_front();
    }
    waiting.push_back(took);
}

/// The render thread presented a frame of `window` the main thread sent,
/// having rasterized its paints in `raster` and presented in `present`,
/// redrawing `damage` of it (0 to 1).
pub fn frame(window: u32, raster: Duration, present: Duration, damage: f32) {
    if !on() {
        return;
    }
    let mut s = store();
    let record = s.recorded.get_mut(&window).and_then(VecDeque::pop_front);
    add(&mut s, window, Frame { record, raster, present, damage: damage.clamp(0.0, 1.0) });
}

/// The window [`offscreen`] frames are counted under: drawings into images
/// rather than windows, on any thread.
pub const OFFSCREEN: u32 = u32::MAX;

/// A drawing into an image (a toolkit's offscreen render) took `record` to
/// record and `raster` to rasterize, all of it: a frame of [`OFFSCREEN`].
pub fn offscreen(record: Duration, raster: Duration) {
    if !on() {
        return;
    }
    let frame = Frame { record: Some(record), raster, present: Duration::ZERO, damage: 1.0 };
    add(&mut store(), OFFSCREEN, frame);
}

/// Count `frame` of `window`, and summarize the period if it's over.
fn add(s: &mut Store, window: u32, frame: Frame) {
    s.frames.entry(window).or_default().push(frame);
    let now = Instant::now();
    let since = *s.since.get_or_insert(now);
    let period = s.period.unwrap_or(PERIOD);
    if now.duration_since(since) >= period {
        s.since = Some(now);
        let took = now.duration_since(since);
        let summaries: Vec<Summary> = {
            let mut windows: Vec<(&u32, &Vec<Frame>)> = s.frames.iter().filter(|(_, f)| !f.is_empty()).collect();
            windows.sort_by_key(|(w, _)| **w);
            windows.into_iter().map(|(w, f)| summarize(*w, f, took)).collect()
        };
        s.frames.clear();
        if printing() {
            for summary in &summaries {
                eprintln!("{}", line(summary));
            }
        }
        s.latest = summaries;
    }
}

/// The window closed: what waited for its frames goes.
pub fn closed(window: u32) {
    if !on() {
        return;
    }
    let mut s = store();
    s.recorded.remove(&window);
    s.frames.remove(&window);
    s.latest.retain(|l| l.window != window);
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    record: Option<Duration>,
    raster: Duration,
    present: Duration,
    damage: f32,
}

#[derive(Default)]
struct Store {
    /// What a toolkit asked for: summaries every period.
    period: Option<Duration>,
    since: Option<Instant>,
    /// Recordings waiting for their frames, by window.
    recorded: HashMap<u32, VecDeque<Duration>>,
    frames: HashMap<u32, Vec<Frame>>,
    latest: Vec<Summary>,
}

fn store() -> std::sync::MutexGuard<'static, Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn micros(d: Duration) -> u32 {
    u32::try_from(d.as_micros()).unwrap_or(u32::MAX)
}

/// The median, 95th percentile (nearest rank) and greatest of `values`.
fn timing(mut values: Vec<u32>) -> Timing {
    if values.is_empty() {
        return Timing::default();
    }
    values.sort_unstable();
    let rank = |p: f64| values[((values.len() as f64 * p).ceil() as usize).clamp(1, values.len()) - 1];
    Timing { p50: rank(0.5), p95: rank(0.95), max: values[values.len() - 1] }
}

fn summarize(window: u32, frames: &[Frame], period: Duration) -> Summary {
    let of = |f: fn(&Frame) -> Duration| timing(frames.iter().map(|x| micros(f(x))).collect());
    let recorded: Vec<u32> = frames.iter().filter_map(|f| f.record.map(micros)).collect();
    Summary {
        window,
        frames: frames.len() as u32,
        record: (!recorded.is_empty()).then(|| timing(recorded)),
        rasterize: of(|f| f.raster),
        present: of(|f| f.present),
        total: of(|f| f.record.unwrap_or_default() + f.raster + f.present),
        damage: frames.iter().map(|f| f.damage).sum::<f32>() / frames.len() as f32,
        period,
    }
}

/// A summary as `SIDESTEP_FRAME_STATS` prints it.
pub fn line(s: &Summary) -> String {
    let t = |t: &Timing| format!("{}/{}/{}", t.p50, t.p95, t.max);
    let record = s.record.as_ref().map_or_else(|| "-".to_owned(), t);
    let window = if s.window == OFFSCREEN { "offscreen".to_owned() } else { format!("window {}", s.window) };
    format!(
        "sidestep frame stats ({:.1} s): {window}: {} frame{}, µs p50/p95/max: record {record}, rasterize {}, \
         present {}, total {}; damage {:.0}%",
        s.period.as_secs_f64(),
        s.frames,
        if s.frames == 1 { "" } else { "s" },
        t(&s.rasterize),
        t(&s.present),
        t(&s.total),
        s.damage * 100.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_rank_the_nearest() {
        assert_eq!(timing(vec![]), Timing::default());
        assert_eq!(timing(vec![7]), Timing { p50: 7, p95: 7, max: 7 });
        let hundred: Vec<u32> = (1..=100).rev().collect();
        assert_eq!(timing(hundred), Timing { p50: 50, p95: 95, max: 100 });
    }

    #[test]
    fn frames_sum_their_parts() {
        let ms = Duration::from_millis;
        let frames = [
            Frame { record: Some(ms(1)), raster: ms(2), present: ms(1), damage: 1.0 },
            Frame { record: None, raster: ms(1), present: ms(1), damage: 0.5 },
        ];
        let s = summarize(3, &frames, ms(5000));
        assert_eq!((s.window, s.frames), (3, 2));
        assert_eq!(s.record, Some(Timing { p50: 1000, p95: 1000, max: 1000 }));
        assert_eq!(s.total, Timing { p50: 2000, p95: 4000, max: 4000 });
        assert_eq!(s.damage, 0.75);
        let printed = line(&s);
        assert!(printed.contains("window 3: 2 frames") && printed.contains("total 2000/4000/4000"), "{printed}");
        assert!(printed.contains("damage 75%"), "{printed}");
        // Offscreen renders are named so.
        assert!(line(&summarize(OFFSCREEN, &frames, ms(5000))).contains("): offscreen: 2 frames"));
    }
}
