//! What a whole window's repaint costs, frame after frame, under the
//! headless sway: a window tiled over the output (1280 × 800 points
//! unless `SIZE` and `SCALE` say otherwise) with plain content (a
//! background, a sidebar, rows of labels, a few rounded rectangles) is
//! drawn whole for three seconds, then only a caret redrawn for three
//! more, and the median and 95th percentile of
//! each part of a frame (recording, rasterizing, presenting) are printed.
//! Run it in a release build, at each scale:
//!
//! ```sh
//! SIZE=1440x900 SCALE=1 scripts/headless-wayland cargo test --release -p sidestep-ui --test frame_budget
//! SIZE=2880x1800 SCALE=2 scripts/headless-wayland cargo test --release -p sidestep-ui --test frame_budget
//! ```
//!
//! It asserts nothing about the times (machines differ), only that frames
//! were measured. Elsewhere it says so and passes.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::time::{Duration, Instant};

    use sidestep_ui::kurbo::{Point, Rect, RoundedRect};
    use sidestep_ui::*;

    const PERIOD: Duration = Duration::from_secs(3);
    const DEADLINE: Duration = Duration::from_secs(60);

    struct Budget {
        started: Instant,
        window: Option<WindowId>,
        /// Whole repaints' statistics, then a caret's (a small rectangle
        /// redrawn each frame).
        stats: Option<FrameStats>,
        small: Option<FrameStats>,
        scale: f64,
        size: sidestep_ui::kurbo::Size,
        measuring: Instant,
    }

    impl Handler for Budget {
        fn launched(&mut self, cx: &mut Cx) {
            self.window = Some(cx.open_window(WindowOptions::new("Frame budget")));
            cx.set_frame_stats(Some(PERIOD));
            cx.set_repeating_timer(Duration::from_millis(50));
        }

        fn draw(&mut self, cx: &mut Cx, window: WindowId, canvas: &mut Canvas) {
            let size = canvas.size();
            (self.scale, self.size) = (canvas.scale(), size);
            canvas.fill_rect(Rect::from_origin_size(Point::ZERO, size), Color::hex(0x1b1d23));
            canvas.fill_rect(Rect::new(0.0, 0.0, 220.0, size.height), Color::hex(0x23262e));
            let label = TextStyle::new(Font::system(13.0), Color::hex(0xd8dce4));
            let dim = TextStyle::new(Font::system(11.0), Color::hex(0x8a90a0));
            let mut y = 12.0;
            while y < size.height - 20.0 {
                canvas.draw_label("Sidebar item", &label, Point::new(16.0, y));
                canvas.draw_label("Soroban — a row of a list, with its details", &label, Point::new(240.0, y));
                canvas.draw_label("12:41", &dim, Point::new(size.width - 60.0, y + 1.0));
                y += 28.0;
            }
            for i in 0..6 {
                let x = 260.0 + f64::from(i) * 140.0;
                canvas.fill(&RoundedRect::new(x, 60.0, x + 120.0, 140.0, 8.0), Color::hex(0x3a6ea5));
            }
            // Whole again at the next frame, or only a caret.
            if let Some(mut w) = cx.window(window) {
                match self.stats {
                    None => w.redraw(),
                    Some(_) => w.invalidate(Rect::new(300.0, 300.0, 302.0, 318.0)),
                }
            }
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            assert!(self.started.elapsed() < DEADLINE, "no frames measured");
            let window = self.window.expect("the window");
            let Some(s) = cx.frame_stats().into_iter().find(|s| s.window == window && s.frames >= 30) else { return };
            if self.stats.is_none() {
                self.stats = Some(s);
                // The caret's, from the next period on.
                cx.set_frame_stats(Some(PERIOD));
                self.measuring = Instant::now();
            } else if self.measuring.elapsed() > PERIOD + Duration::from_millis(500) && s.damage < 0.01 {
                self.small = Some(s);
                cx.quit();
            }
        }
    }

    pub fn run() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var("WLR_BACKENDS").as_deref() != Ok("headless") {
            println!("skipped: needs the headless sway (run it under scripts/headless-wayland)");
            return;
        }
        // SAFETY: nothing else runs yet.
        unsafe { std::env::set_var("SIDESTEP_APPEARANCE", "dark") };
        let budget = Budget {
            started: Instant::now(),
            window: None,
            stats: None,
            small: None,
            scale: 0.0,
            size: Default::default(),
            measuring: Instant::now(),
        };
        let budget = App::new().run(budget).expect("a compositor");
        let us = |d: Duration| d.as_secs_f64() * 1e6;
        let t = |t: &FrameTiming| format!("{:6.0} {:6.0}", us(t.p50), us(t.p95));
        for (what, s) in
            [("whole", budget.stats.expect("frames measured")), ("caret", budget.small.expect("frames measured"))]
        {
            println!(
                "frame budget: {} × {} points at {}×, {what}: {} frames, µs p50 p95: record {}, rasterize {}, present {}, total {}",
                budget.size.width,
                budget.size.height,
                budget.scale,
                s.frames,
                t(&s.record),
                t(&s.rasterize),
                t(&s.present),
                t(&s.total)
            );
        }
    }
}
