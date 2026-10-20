//! Rasterizing a large paint on several threads at once: the canvas is
//! cut into bands of whole rows, and each band draws every op, clipped to
//! its rows, on a thread of its own (the caller's and a few workers', each
//! with a glyph cache of its own). A pixel's value doesn't depend on how
//! the canvas or the damage is cut up, so the pixels are the ones one
//! thread draws; ops that read pixels outside their own (a shadow's blur)
//! or that need an image cache (images, image masks) keep a paint on one
//! thread, as small paints stay, where waking workers would cost more than
//! it saves.
//!
//! The workers are started the first time they're wanted: as many as the
//! machine has cores beyond one, up to three (`SIDESTEP_RASTER_THREADS`
//! sets how many threads draw in all; 1 draws on the caller's alone).

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use super::images::{ImageData, Pixels};
use super::{Canvas, Glyphs, paint};
use crate::protocol::{Draw, Op, Rect};

/// The fewest pixels a paint covers for its bands to be drawn on several
/// threads.
pub const BANDED: usize = 1 << 18;

type Job = Box<dyn FnOnce() + Send>;

struct Pool {
    jobs: Mutex<Vec<Job>>,
    wake: Condvar,
    workers: usize,
}

thread_local!(static GLYPHS: RefCell<Glyphs> = RefCell::default());

/// The workers, started the first time.
fn pool() -> &'static Pool {
    static POOL: OnceLock<&'static Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let threads = std::env::var("SIDESTEP_RASTER_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(|| cores.min(4))
            .max(1);
        let pool: &'static Pool =
            Box::leak(Box::new(Pool { jobs: Mutex::new(Vec::new()), wake: Condvar::new(), workers: threads - 1 }));
        for n in 0..pool.workers {
            let started = std::thread::Builder::new().name(format!("sidestep-raster-{n}")).spawn(move || {
                loop {
                    let job = {
                        let mut jobs = pool.jobs.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if let Some(job) = jobs.pop() {
                                break job;
                            }
                            jobs = pool.wake.wait(jobs).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    job();
                }
            });
            if started.is_err() {
                // Fewer workers then: the caller draws the bands left.
                break;
            }
        }
        pool
    })
}

/// Counts the bands still drawing, and whether one panicked.
struct Latch {
    left: Mutex<usize>,
    done: Condvar,
    panicked: AtomicBool,
}

impl Latch {
    fn finished(&self) {
        let mut left = self.left.lock().unwrap_or_else(|e| e.into_inner());
        *left -= 1;
        if *left == 0 {
            self.done.notify_all();
        }
    }

    fn wait(&self) {
        let mut left = self.left.lock().unwrap_or_else(|e| e.into_inner());
        while *left > 0 {
            left = self.done.wait(left).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Waits for the bands handed to workers however the caller leaves (a
/// panic in its own band included): they borrow its canvas.
struct WaitFor(Arc<Latch>);

impl Drop for WaitFor {
    fn drop(&mut self) {
        self.0.wait();
    }
}

/// The most pixels an image has for a paint drawing it to go in bands:
/// each band's thread keeps it in its own image cache (a tinted copy, a
/// halving), which for icons and the like costs next to nothing.
const SMALL_IMAGE: u32 = 256 * 256;

/// Whether every op draws its pixels from nothing but itself and what's
/// under them, with no image cache but for small images whose pixels are
/// shared (icons, glyph-like pictures): each band's thread caches its own
/// copies of those, where large ones and files to decode keep a paint on
/// one thread.
fn bandable(ops: &[Op]) -> bool {
    let plain = |d: &Draw| d.shadow.is_none() && d.mask.iter().flat_map(|m| m.iter()).all(|c| c.image.is_none());
    let small = |i: &ImageData| matches!(i.pixels, Pixels::Rgba(_)) && i.width.saturating_mul(i.height) <= SMALL_IMAGE;
    ops.iter().all(|op| match op {
        Op::Fill { .. } | Op::FillWith { .. } | Op::Glyphs(_) | Op::EndGroup => true,
        Op::FillPath { draw, .. } | Op::StrokePath { draw, .. } | Op::BeginGroup { draw, .. } => plain(draw),
        Op::Image { image, draw, .. } => plain(draw) && small(image),
        Op::Composite(_) => false,
    })
}

/// [`paint`], in bands on several threads when `rects` cover enough of the
/// canvas and `ops` allow it; the same pixels either way.
pub fn paint_banded(canvas: &mut Canvas, glyphs: &mut Glyphs, rects: &[Rect], ops: &[Op]) {
    let covered: Vec<(usize, usize, usize, usize)> = rects.iter().filter_map(|r| canvas.pixels(r)).collect();
    let area: usize = covered.iter().map(|(x0, y0, x1, y1)| (x1 - x0) * (y1 - y0)).sum();
    let (Some(top), Some(bottom)) = (covered.iter().map(|c| c.1).min(), covered.iter().map(|c| c.3).max()) else {
        return;
    };
    if area < BANDED || !bandable(ops) {
        paint(canvas, glyphs, rects, ops);
        return;
    }
    let pool = pool();
    // Rows top..bottom, cut into bands of nearly equal height where no path
    // is drawn across the cut.
    let paths: Vec<(usize, usize)> =
        ops.iter().flat_map(|op| rects.iter().filter_map(|r| super::ops::path_rows(canvas, r, op))).collect();
    let cuts = cuts(top, bottom, pool.workers + 1, &paths);
    if cuts.len() <= 2 {
        paint(canvas, glyphs, rects, ops);
        return;
    }
    let bands = cuts.len() - 1;
    let stride = canvas.stride;
    let (width, scale, origin_y, x0, y0) = (canvas.width, canvas.scale, canvas.origin_y, canvas.x0, canvas.y0);
    let (_, rest) = canvas.px.split_at_mut(top * stride);
    let mut rest = rest;
    let mut canvases = Vec::with_capacity(bands);
    for pair in cuts.windows(2) {
        let (from, rows) = (pair[0], pair[1] - pair[0]);
        let (band, after) = rest.split_at_mut((rows * stride).min(rest.len()));
        rest = after;
        canvases.push(Canvas {
            px: band,
            width,
            height: rows as u32,
            stride,
            origin_y,
            scale,
            x0,
            y0: y0 + from as i32,
        });
    }
    let mut mine = canvases.remove(0);
    let others: Vec<Box<dyn FnOnce() + Send + '_>> = canvases
        .into_iter()
        .map(|mut band| -> Box<dyn FnOnce() + Send + '_> {
            Box::new(move || {
                GLYPHS.with(|g| match g.try_borrow_mut() {
                    Ok(mut glyphs) => paint(&mut band, &mut glyphs, rects, ops),
                    // A band taken while this thread draws another's.
                    Err(_) => paint(&mut band, &mut Glyphs::default(), rects, ops),
                })
            })
        })
        .collect();
    scoped(others, || paint(&mut mine, glyphs, rects, ops));
}

/// How many workers share work with a caller.
pub fn workers() -> usize {
    pool().workers
}

/// Run `others` on the workers and `mine` on the caller, and return once
/// they're all done (the caller runs those no worker has taken by then):
/// they may borrow what the caller has.
pub fn scoped<'a>(others: Vec<Box<dyn FnOnce() + Send + 'a>>, mine: impl FnOnce()) {
    let pool = pool();
    let latch =
        Arc::new(Latch { left: Mutex::new(others.len()), done: Condvar::new(), panicked: AtomicBool::new(false) });
    let wait = WaitFor(latch.clone());
    {
        let mut jobs = pool.jobs.lock().unwrap_or_else(|e| e.into_inner());
        for work in others {
            let latch = latch.clone();
            let job: Box<dyn FnOnce() + Send + 'a> = Box::new(move || {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
                    latch.panicked.store(true, Ordering::Relaxed);
                }
                latch.finished();
            });
            // SAFETY: the job borrows only what outlives it: `wait` waits
            // for every job handed out before this function returns or
            // unwinds.
            let job: Job = unsafe { std::mem::transmute::<Box<dyn FnOnce() + Send + 'a>, Job>(job) };
            jobs.push(job);
        }
    }
    pool.wake.notify_all();
    mine();
    loop {
        let job = pool.jobs.lock().unwrap_or_else(|e| e.into_inner()).pop();
        let Some(job) = job else { break };
        job();
    }
    drop(wait);
    assert!(!latch.panicked.load(Ordering::Relaxed), "sidestep: a raster worker panicked");
}

/// Where to cut rows `top..bottom` into `bands` bands of about the same
/// height, no cut inside one of `paths`' row ranges: the cuts, `top`
/// first and `bottom` last, at least 16 rows apart.
fn cuts(top: usize, bottom: usize, bands: usize, paths: &[(usize, usize)]) -> Vec<usize> {
    // The rows paths cover, overlapping ranges merged.
    let mut spans: Vec<(usize, usize)> = paths.to_vec();
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (y0, y1) in spans {
        match merged.last_mut() {
            Some(last) if y0 < last.1 => last.1 = last.1.max(y1),
            _ => merged.push((y0, y1)),
        }
    }
    let mut cuts = vec![top];
    for b in 1..bands {
        let ideal = top + (bottom - top) * b / bands;
        // Out of the paths spanning it, to the nearer side.
        let at = match merged.iter().find(|(y0, y1)| *y0 < ideal && ideal < *y1) {
            Some(&(y0, y1)) if ideal - y0 <= y1 - ideal => y0,
            Some(&(_, y1)) => y1,
            None => ideal,
        };
        if at >= cuts[cuts.len() - 1] + 16 && at + 16 <= bottom {
            cuts.push(at);
        }
    }
    cuts.push(bottom);
    cuts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Blend, Paint};

    #[test]
    fn cuts_keep_out_of_paths() {
        assert_eq!(cuts(0, 400, 4, &[]), [0, 100, 200, 300, 400]);
        // Out of a path, to its nearer edge; overlapping paths are one.
        assert_eq!(cuts(0, 400, 4, &[(90, 120), (150, 230), (220, 260)]), [0, 90, 150, 300, 400]);
        // No band thinner than 16 rows.
        assert_eq!(cuts(0, 400, 4, &[(10, 390)]), [0, 400]);
        assert_eq!(cuts(0, 40, 4, &[]), [0, 20, 40]);
    }

    /// Shapes, fills and groups drawn in bands on several threads are the
    /// pixels one thread draws, at whole and fractional scales.
    #[test]
    fn bands_draw_what_one_thread_draws() {
        let ops = |w: f32, h: f32| -> Vec<Op> {
            let mut ops = vec![
                Op::FillWith { rect: Rect::new(0.0, 0.0, w, h), color: [0.1, 0.1, 0.1, 1.0], blend: Blend::Copy },
                Op::Fill { rect: Rect::new(3.3, 7.7, w - 5.1, h - 2.9), color: [0.2, 0.5, 0.9, 0.6] },
            ];
            for i in 0..12 {
                // Two rows of circles, with room between and under them.
                let (x, y) = (10.0 + (i % 6) as f32 * 97.0, 40.0 + (i / 6) as f32 * 180.0);
                let mut pb = tiny_skia::PathBuilder::new();
                pb.push_circle(x, y, 23.7);
                let path = std::sync::Arc::new(pb.finish().expect("a circle"));
                let draw = Draw {
                    xf: tiny_skia::Transform::from_rotate(i as f32 * 3.0),
                    blend: Blend::SourceOver,
                    aa: true,
                    clip: Rect::new(0.0, 0.0, w, h),
                    mask: None,
                    shadow: None,
                };
                if i % 4 == 0 {
                    ops.push(Op::BeginGroup { alpha: 0.5, draw: draw.clone() });
                }
                ops.push(Op::FillPath { path, even_odd: false, paint: Paint::Solid([0.9, 0.3, 0.1, 0.8]), draw });
                if i % 4 == 0 {
                    ops.push(Op::EndGroup);
                }
            }
            ops
        };
        for scale in [1.0f32, 1.5, 2.0] {
            let (w, h) = (640.0f32, 513.0f32);
            let (pw, ph) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
            let ops = ops(w, h);
            let damage = [Rect::new(0.0, 0.0, w, h)];
            // Drawn in bands, cut between the circles.
            let probe = Canvas::new(&mut [], pw, ph, 0.0, scale);
            let paths: Vec<(usize, usize)> =
                ops.iter().filter_map(|op| super::super::ops::path_rows(&probe, &damage[0], op)).collect();
            assert!(cuts(0, ph as usize, 4, &paths).len() > 2, "{paths:?}");
            let mut one = vec![0u32; (pw * ph) as usize];
            paint(&mut Canvas::new(&mut one, pw, ph, 0.0, scale), &mut Glyphs::default(), &damage, &ops);
            let mut banded = vec![0u32; (pw * ph) as usize];
            paint_banded(&mut Canvas::new(&mut banded, pw, ph, 0.0, scale), &mut Glyphs::default(), &damage, &ops);
            let differ = one.iter().zip(&banded).filter(|(a, b)| a != b).count();
            assert_eq!(differ, 0, "{differ} pixels differ at scale {scale}");
            // Part of it: only the damage's rows, in bands.
            let part = [Rect::new(0.0, 101.5, w, 450.25)];
            paint_banded(&mut Canvas::new(&mut banded, pw, ph, 0.0, scale), &mut Glyphs::default(), &part, &ops[2..]);
            paint(&mut Canvas::new(&mut one, pw, ph, 0.0, scale), &mut Glyphs::default(), &part, &ops[2..]);
            assert!(one == banded, "a part, at scale {scale}");
        }
    }

    /// Small images (icons: tinted, scaled smooth or not, one to one,
    /// translucent) drawn in bands are the pixels one thread draws; a large
    /// one keeps the paint on one thread.
    #[test]
    fn small_images_draw_in_bands_as_on_one_thread() {
        use crate::protocol::Quality;
        use crate::raster::images::next_key;
        let icon = |w: u32, h: u32| {
            let px: Vec<u8> = (0..w * h)
                .flat_map(|i| {
                    let (x, y) = (i % w, i / w);
                    let a = if (x + y) % 5 == 0 { 0 } else { 40 + ((x * 7 + y * 3) % 200) as u8 };
                    [a / 2, a / 3, a, a]
                })
                .collect();
            Arc::new(ImageData { key: next_key(), generation: 0, width: w, height: h, pixels: Pixels::Rgba(px.into()) })
        };
        let small = icon(24, 24);
        let draw = |w: f32, h: f32| Draw {
            xf: tiny_skia::Transform::identity(),
            blend: Blend::SourceOver,
            aa: true,
            clip: Rect::new(0.0, 0.0, w, h),
            mask: None,
            shadow: None,
        };
        let ops = |w: f32, h: f32| -> Vec<Op> {
            let mut ops =
                vec![Op::FillWith { rect: Rect::new(0.0, 0.0, w, h), color: [0.1, 0.1, 0.1, 1.0], blend: Blend::Copy }];
            for i in 0..40 {
                let (x, y) = (7.3 + (i % 8) as f32 * 61.0, 11.0 + (i / 8) as f32 * 97.0);
                let size = if i % 3 == 0 { 12.0 } else { 16.5 + (i % 4) as f32 * 7.25 };
                let quality = [Quality::None, Quality::Low, Quality::High][i % 3];
                ops.push(Op::Image {
                    image: small.clone(),
                    src: Rect::new(0.0, 0.0, 24.0, 24.0),
                    dst: Rect::new(x, y, x + size, y + size),
                    alpha: if i % 5 == 0 { 0.6 } else { 1.0 },
                    quality,
                    tint: (i % 4 == 1).then_some([0.9, 0.6, 0.2, 1.0]),
                    tiled: false,
                    draw: draw(w, h),
                });
            }
            ops
        };
        for scale in [1.0f32, 1.5, 2.0] {
            let (w, h) = (520.0f32, 513.0f32);
            let (pw, ph) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
            let ops = ops(w, h);
            assert!(bandable(&ops));
            let damage = [Rect::new(0.0, 0.0, w, h)];
            let mut one = vec![0u32; (pw * ph) as usize];
            paint(&mut Canvas::new(&mut one, pw, ph, 0.0, scale), &mut Glyphs::default(), &damage, &ops);
            let mut banded = vec![0u32; (pw * ph) as usize];
            paint_banded(&mut Canvas::new(&mut banded, pw, ph, 0.0, scale), &mut Glyphs::default(), &damage, &ops);
            let differ = one.iter().zip(&banded).filter(|(a, b)| a != b).count();
            assert_eq!(differ, 0, "{differ} pixels differ at scale {scale}");
        }
        let large = icon(600, 400);
        let mut big = ops(520.0, 513.0);
        if let Op::Image { image, .. } = &mut big[1] {
            *image = large;
        }
        assert!(!bandable(&big), "a large image keeps the paint on one thread");
    }
}
