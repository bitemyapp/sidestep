//! What a window's frames cost, frame after frame, under the headless
//! sway: a window tiled over the output (1280 × 800 points unless `SIZE`
//! and `SCALE` say otherwise) with plain content (a sidebar, a list of
//! rows with labels and rounded icons) is drawn for three seconds in each
//! of four ways: whole each frame; only a caret; its list scrolled by a
//! wheel's notch (40 points) each frame, redrawn whole; and scrolled by
//! moving its pixels (`Window::scroll`), drawing only the strip
//! uncovered. The median and 95th percentile of each part of a frame
//! (recording, rasterizing, presenting) are printed for each. Run it in a
//! release build, at each scale:
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

    use sidestep_ui::kurbo::{Point, Rect, RoundedRect, Size, Vec2};
    use sidestep_ui::*;

    const PERIOD: Duration = Duration::from_secs(3);
    const DEADLINE: Duration = Duration::from_secs(90);
    const ROW: f64 = 28.0;
    const NOTCH: f64 = 40.0;

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Phase {
        Whole,
        Caret,
        ScrollRedrawn,
        ScrollMoved,
    }

    const PHASES: [Phase; 4] = [Phase::Whole, Phase::Caret, Phase::ScrollRedrawn, Phase::ScrollMoved];

    struct Budget {
        started: Instant,
        window: Option<WindowId>,
        phase: usize,
        offset: f64,
        results: Vec<(Phase, FrameStats)>,
        scale: f64,
        size: Size,
    }

    fn list(size: Size) -> Rect {
        Rect::new(220.0, 0.0, size.width, size.height)
    }

    fn draw(canvas: &mut Canvas, offset: f64) {
        let size = canvas.size();
        canvas.fill_rect(Rect::new(0.0, 0.0, 220.0, size.height), Color::hex(0x23262e));
        let label = TextStyle::new(Font::system(13.0), Color::hex(0xd8dce4));
        let dim = TextStyle::new(Font::system(11.0), Color::hex(0x8a90a0));
        let mut y = 12.0;
        while y < size.height - 20.0 {
            if canvas.needs(Rect::new(0.0, y, 220.0, y + ROW)) {
                canvas.draw_label("Sidebar item", &label, Point::new(16.0, y));
            }
            y += ROW;
        }
        let list = list(size);
        canvas.save();
        canvas.clip_rect(list);
        canvas.fill_rect(list, Color::hex(0x1b1d23));
        let first = (offset / ROW).floor().max(0.0) as i64;
        for i in first.. {
            let y = i as f64 * ROW - offset;
            if y > list.y1 {
                break;
            }
            if !canvas.needs(Rect::new(list.x0, y, list.x1, y + ROW)) {
                continue;
            }
            if i % 2 == 1 {
                canvas.fill_rect(Rect::new(list.x0, y, list.x1, y + ROW), Color::hex(0x20232a));
            }
            canvas.fill(&RoundedRect::new(232.0, y + 5.0, 250.0, y + 23.0, 4.0), Color::hex(0x3a6ea5));
            let title = format!("Soroban — row {i} of a list, with its details");
            canvas.draw_label(&title, &label, Point::new(260.0, y + 6.0));
            canvas.draw_label("12:41", &dim, Point::new(list.x1 - 60.0, y + 8.0));
        }
        canvas.restore();
    }

    impl Handler for Budget {
        fn launched(&mut self, cx: &mut Cx) {
            self.window = Some(cx.open_window(WindowOptions::new("Frame budget")));
            cx.set_frame_stats(Some(PERIOD));
            cx.set_repeating_timer(Duration::from_millis(50));
        }

        fn draw(&mut self, cx: &mut Cx, window: WindowId, canvas: &mut Canvas) {
            (self.scale, self.size) = (canvas.scale(), canvas.size());
            draw(canvas, self.offset);
            // The next frame, as the phase draws it.
            let Some(mut w) = cx.window(window) else { return };
            let list = list(self.size);
            match PHASES[self.phase.min(PHASES.len() - 1)] {
                Phase::Whole => w.redraw(),
                Phase::Caret => w.invalidate(Rect::new(300.0, 300.0, 302.0, 318.0)),
                Phase::ScrollRedrawn => {
                    self.offset += NOTCH;
                    w.invalidate(list);
                }
                Phase::ScrollMoved => {
                    self.offset += NOTCH;
                    w.scroll(list, Vec2::new(0.0, -NOTCH));
                }
            }
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            assert!(self.started.elapsed() < DEADLINE, "no frames measured");
            let window = self.window.expect("the window");
            let Some(s) = cx.frame_stats().into_iter().find(|s| s.window == window && s.frames >= 30) else { return };
            self.results.push((PHASES[self.phase], s));
            self.phase += 1;
            if self.phase == PHASES.len() {
                cx.quit();
                return;
            }
            // The next phase's frames alone, from the next period.
            cx.set_frame_stats(Some(PERIOD));
            if let Some(mut w) = cx.window(window) {
                w.redraw();
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
            phase: 0,
            offset: 0.0,
            results: Vec::new(),
            scale: 0.0,
            size: Size::ZERO,
        };
        let budget = App::new().run(budget).expect("a compositor");
        let us = |d: Duration| d.as_secs_f64() * 1e6;
        let t = |t: &FrameTiming| format!("{:6.0} {:6.0}", us(t.p50), us(t.p95));
        for (phase, s) in &budget.results {
            println!(
                "frame budget: {} × {} points at {}×, {phase:?}: {} frames, µs p50 p95: record {}, rasterize {}, \
                 present {}, total {}",
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
        assert_eq!(budget.results.len(), PHASES.len());
    }
}
