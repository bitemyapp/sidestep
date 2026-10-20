//! Scrolling by moving pixels, through the null render thread (which
//! keeps a window's pixels and moves them as a compositor's buffer is):
//! a list scrolled notch by notch, two notches between passes added up,
//! up and down, shows what drawing it whole at its last offset draws
//! offscreen, at scales 1 and 2 (exactly, but at the rounded corners of
//! icons a strip cut through, antialiased a little differently); each
//! pass redraws only the strip a move uncovers; a move by part of a
//! pixel, or of all of the list, redraws the list instead. (The list is
//! large enough for its pixels to be moved on the raster workers, in
//! bands of columns.)

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::time::{Duration, Instant};

    use sidestep_ui::kurbo::{Point, Rect, RoundedRect, Size, Vec2};
    use sidestep_ui::*;

    const SIZE: Size = Size::new(640.0, 480.0);
    /// The list, under a header that doesn't scroll.
    const LIST: Rect = Rect::new(0.0, 40.0, 640.0, 480.0);
    const ROW: f64 = 22.0;
    const DEADLINE: Duration = Duration::from_secs(30);

    /// The window: a header, and the list scrolled to `offset`.
    pub fn draw(canvas: &mut Canvas, offset: f64) {
        canvas.fill_rect(Rect::new(0.0, 0.0, SIZE.width, LIST.y0), Color::hex(0x30343c));
        let label = TextStyle::new(Font::system(12.0), Color::hex(0xe0e4ea));
        canvas.draw_label("Header", &label, Point::new(8.0, 12.0));
        canvas.save();
        canvas.clip_rect(LIST);
        canvas.fill_rect(LIST, Color::hex(0x1b1d23));
        let first = ((offset / ROW).floor() as i64).max(0);
        for i in first.. {
            let y = LIST.y0 + i as f64 * ROW - offset;
            if y > LIST.y1 {
                break;
            }
            let row = Rect::new(0.0, y, SIZE.width, y + ROW);
            if !canvas.needs(row) {
                continue;
            }
            if i % 2 == 1 {
                canvas.fill_rect(row, Color::hex(0x22252c));
            }
            canvas.fill(&RoundedRect::new(6.0, y + 3.0, 22.0, y + 19.0, 4.0), Color::hex(0x3a6ea5));
            canvas.draw_label(&format!("Row {i}, its title"), &label, Point::new(30.0, y + 4.0));
        }
        canvas.restore();
    }

    struct List {
        started: Instant,
        window: Option<WindowId>,
        offset: f64,
        /// The damage each pass drew.
        drawn: Vec<Vec<Rect>>,
        step: usize,
    }

    impl List {
        fn scroll(&mut self, cx: &mut Cx, by: f64) {
            self.offset += by;
            let window = self.window.expect("the window");
            cx.window(window).expect("open").scroll(LIST, Vec2::new(0.0, -by));
        }
    }

    impl Handler for List {
        fn launched(&mut self, cx: &mut Cx) {
            let options = WindowOptions::new("Scrolling").size(SIZE.width, SIZE.height);
            self.window = Some(cx.open_window(options));
            cx.set_repeating_timer(Duration::from_millis(2));
        }

        fn draw(&mut self, _cx: &mut Cx, _window: WindowId, canvas: &mut Canvas) {
            self.drawn.push(canvas.damage());
            draw(canvas, self.offset);
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            assert!(self.started.elapsed() < DEADLINE, "step {} didn't finish", self.step);
            let window = self.window.expect("the window");
            if self.drawn.len() <= self.step || !testing::render_idle() {
                return;
            }
            let scale = cx.window(window).expect("open").scale();
            let strip_only = |drawn: &[Rect]| {
                let reach = drawn.iter().fold(Rect::ZERO, |a, r| if a.is_zero_area() { *r } else { a.union(*r) });
                assert!(reach.area() <= SIZE.width * 51.0, "only the strip: {drawn:?}");
            };
            match self.step {
                0 => self.scroll(cx, 33.0),
                // Part of a pixel at this scale: the list is redrawn.
                1 => self.scroll(cx, 1.0 / (2.0 * scale)),
                2 => {
                    let damage = &self.drawn[2];
                    assert!(damage.iter().any(|r| r.contains(Point::new(320.0, 400.0))), "redrawn: {damage:?}");
                    self.scroll(cx, -1.0 / (2.0 * scale));
                }
                // More than the list is tall: redrawn.
                3 => self.scroll(cx, 500.0),
                // Then notches down, each redrawing only what it uncovers,
                // two before a pass added up, and up again: the pixels the
                // list ends with were moved, not drawn whole.
                4..=8 => {
                    if self.step > 4 {
                        strip_only(&self.drawn[self.step]);
                    }
                    self.scroll(cx, 33.0);
                }
                9 => {
                    strip_only(&self.drawn[9]);
                    self.scroll(cx, 30.0);
                    self.scroll(cx, 14.0);
                }
                10 => {
                    strip_only(&self.drawn[10]);
                    self.scroll(cx, -51.0);
                }
                _ => {
                    strip_only(&self.drawn[11]);
                    let (w, h, px) = testing::window_pixels(window).expect("captured");
                    let expected = offscreen::render(SIZE, scale, Appearance::LIGHT, |c| {
                        c.fill_rect(Rect::from_origin_size(Point::ZERO, SIZE), Color::WHITE);
                        draw(c, self.offset);
                    });
                    assert_eq!(expected.pixel_size(), (w, h));
                    let expected = expected.to_premultiplied_rgba8().expect("pixels");
                    // The same pixels, but where a strip cut through an icon's
                    // rounded corner: tiny-skia's antialiasing of a path cut off
                    // by a clip comes out some levels from the path drawn
                    // whole, as any partial redraw does.
                    let pixels = px.concat();
                    let differ: Vec<usize> = (0..pixels.len() / 4)
                        .filter(|i| pixels[i * 4..i * 4 + 4] != expected[i * 4..i * 4 + 4])
                        .collect();
                    for &i in &differ {
                        let (x, near) = ((i % w as usize) as f64 / scale, |a: u8, b: u8| a.abs_diff(b) <= 48);
                        let same = (0..4).all(|c| near(pixels[i * 4 + c], expected[i * 4 + c]));
                        assert!(
                            same && (6.0..22.0).contains(&x),
                            "pixel {i} differs past an icon's edge: {:?} {:?}",
                            &pixels[i * 4..i * 4 + 4],
                            &expected[i * 4..i * 4 + 4]
                        );
                    }
                    assert!(differ.len() < 64, "{} pixels differ", differ.len());
                    cx.quit();
                    return;
                }
            }
            self.step += 1;
        }
    }

    pub fn run() {
        // SAFETY: nothing else runs yet.
        unsafe {
            std::env::set_var("SIDESTEP_APPEARANCE", "light");
            std::env::remove_var("WAYLAND_DISPLAY");
        }
        testing::use_null_backend();
        testing::capture_pixels(true);
        for scale in [1, 2] {
            testing::use_null_backend_scale(scale);
            let list = List { started: Instant::now(), window: None, offset: 0.0, drawn: Vec::new(), step: 0 };
            let list = App::new().run(list).expect("the null render thread");
            println!("  scale {scale}: {} passes, at {}", list.drawn.len(), list.offset);
        }
        println!("scrolling: ok");
    }
}
