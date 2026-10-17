//! Offscreen drawing against a window's: a label drawn into a window
//! through the null render thread (which keeps its pixels) and the same
//! label drawn offscreen at the same scale, at 1 and at 2, come out pixel
//! for pixel the same; and drawing offscreen needs no application, before
//! one runs or after.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::time::{Duration, Instant};

    use sidestep_ui::kurbo::{Point, Rect, Size};
    use sidestep_ui::*;

    const SIZE: Size = Size::new(180.0, 30.0);
    const DEADLINE: Duration = Duration::from_secs(30);

    fn draw(canvas: &mut Canvas) {
        let style = TextStyle::new(Font::system(13.0), Color::BLACK);
        canvas.draw_label("Haiku tab — Soroban", &style, Point::new(6.0, 5.5));
        let bold = TextStyle::new(Font::system(11.0).bold(), Color::hex(0x8a5a00));
        canvas.draw_label("12:41", &bold, Point::new(140.3, 8.0));
    }

    /// Opens a window, and quits once its first frame showed.
    struct Shown {
        window: Option<WindowId>,
        drawn: bool,
        started: Instant,
    }

    impl Handler for Shown {
        fn launched(&mut self, cx: &mut Cx) {
            let options = WindowOptions::new("Offscreen").size(SIZE.width, SIZE.height);
            self.window = Some(cx.open_window(options.background(Background::Color(Color::WHITE))));
            cx.set_repeating_timer(Duration::from_millis(2));
        }

        fn draw(&mut self, _cx: &mut Cx, _window: WindowId, canvas: &mut Canvas) {
            draw(canvas);
            self.drawn = true;
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            assert!(self.started.elapsed() < DEADLINE, "the window never drew");
            if self.drawn && testing::render_idle() {
                cx.quit();
            }
        }
    }

    fn window_pixels(scale: u32) -> Vec<u8> {
        testing::use_null_backend_scale(scale);
        let shown = App::new().run(Shown { window: None, drawn: false, started: Instant::now() }).expect("runs");
        let (w, h, px) = testing::window_pixels(shown.window.expect("a window")).expect("captured");
        assert_eq!((w, h), ((SIZE.width as u32) * scale, (SIZE.height as u32) * scale));
        px.concat()
    }

    fn offscreen_pixels(scale: u32) -> Vec<u8> {
        let image = offscreen::render(SIZE, f64::from(scale), Appearance::LIGHT, |c| {
            c.fill_rect(Rect::from_origin_size(Point::ZERO, SIZE), Color::WHITE);
            draw(c);
        });
        image.to_premultiplied_rgba8().expect("pixels")
    }

    pub fn run() {
        // SAFETY: nothing else runs yet.
        unsafe {
            std::env::set_var("SIDESTEP_APPEARANCE", "light");
            std::env::remove_var("WAYLAND_DISPLAY");
        }
        // Before any application.
        let before = offscreen_pixels(2);
        testing::use_null_backend();
        testing::capture_pixels(true);
        for scale in [1, 2] {
            let window = window_pixels(scale);
            let image = offscreen_pixels(scale);
            assert_eq!(window.len(), image.len());
            let differ = window.chunks(4).zip(image.chunks(4)).filter(|(a, b)| a != b).count();
            assert_eq!(differ, 0, "{differ} pixels differ at scale {scale}");
            let inked = image.chunks(4).filter(|p| p[0] < 200).count();
            assert!(inked > 50 * scale as usize, "the labels drew: {inked}");
        }
        // And after.
        assert!(offscreen_pixels(2) == before, "the same before an application and after");
        println!("offscreen: ok");
    }
}
