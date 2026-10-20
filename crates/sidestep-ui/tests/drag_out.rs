//! Dragging out of a window, through the null render thread: a drag
//! started from a press offers its types and actions with an icon, a
//! destination's request for promised data is answered by the handler
//! (and only for that drag), and the handler hears how it ended.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::time::{Duration, Instant};

    use sidestep_ui::kurbo::{Point, Size};
    use sidestep_ui::testing::{self, Seen};
    use sidestep_ui::*;

    const DEADLINE: Duration = Duration::from_secs(30);

    #[derive(Default)]
    struct Test {
        started: Option<Instant>,
        window: Option<WindowId>,
        drag: Option<DragId>,
        asked: Vec<(DragId, String)>,
        ended: Vec<(DragId, DropAction)>,
        log: Vec<Seen>,
        step: usize,
    }

    impl Handler for Test {
        fn launched(&mut self, cx: &mut Cx) {
            self.started = Some(Instant::now());
            self.window = Some(cx.open_window(WindowOptions::new("Drag out").size(200.0, 100.0)));
            cx.set_repeating_timer(Duration::from_millis(2));
        }

        fn window_event(&mut self, cx: &mut Cx, window: WindowId, event: WindowEvent) {
            // A press starts the drag.
            if matches!(event, WindowEvent::PointerButton { pressed: true, .. }) {
                let icon = offscreen::render(Size::new(24.0, 16.0), 1.0, Appearance::LIGHT, |c| {
                    c.fill_rect(sidestep_ui::kurbo::Rect::new(0.0, 0.0, 24.0, 16.0), Color::hex(0xffa300));
                });
                let source = DragSource::new()
                    .text("dragged text")
                    .urls(["file:///home/user/report.pdf"])
                    .promise("image/png")
                    .actions(true, true)
                    .icon(icon, Point::new(12.0, 8.0));
                self.drag = cx.start_drag(window, source);
            }
        }

        fn draw(&mut self, _cx: &mut Cx, _window: WindowId, _canvas: &mut Canvas) {}

        fn provide_drag_data(&mut self, _cx: &mut Cx, drag: DragId, mime: &str) -> Option<Vec<u8>> {
            self.asked.push((drag, mime.to_owned()));
            (mime == "image/png").then(|| b"\x89PNG made on demand".to_vec())
        }

        fn drag_ended(&mut self, _cx: &mut Cx, drag: DragId, action: DropAction) {
            self.ended.push((drag, action));
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            assert!(self.started.expect("launched").elapsed() < DEADLINE, "step {} didn't finish", self.step);
            self.log.extend(testing::take_render_log());
            let window = self.window.expect("the window");
            match self.step {
                0 => {
                    if testing::showing_id(cx, window).is_none() || !testing::render_idle() {
                        return;
                    }
                    testing::inject_button(cx, window, Point::new(50.0, 50.0), PointerButton::Left, true, 1);
                    self.step = 1;
                }
                1 => {
                    let Some(drag) = self.drag else { return };
                    let started = self.log.iter().find_map(|s| match s {
                        Seen::DragStarted { mimes, actions, icon, .. } => Some((mimes.clone(), *actions, *icon)),
                        _ => None,
                    });
                    let Some((mimes, actions, icon)) = started else { return };
                    assert!(mimes.contains(&"text/plain;charset=utf-8".to_owned()), "{mimes:?}");
                    assert!(mimes.contains(&"text/uri-list".to_owned()) && mimes.contains(&"image/png".to_owned()));
                    assert_eq!((actions, icon), (1 | 2, Some((24, 16))));
                    // A destination asks for the promised type.
                    testing::inject_drag_data_request(cx, drag, "image/png", 7);
                    self.step = 2;
                }
                2 => {
                    let provided = self.log.iter().find_map(|s| match s {
                        Seen::Provided { token: 7, data } => Some(data.clone()),
                        _ => None,
                    });
                    let Some(data) = provided else { return };
                    assert_eq!(data.as_deref(), Some(&b"\x89PNG made on demand"[..]));
                    assert_eq!(self.asked, [(self.drag.expect("a drag"), "image/png".to_owned())]);
                    testing::inject_drag_ended(cx, self.drag.expect("a drag"), DropAction::Move);
                    self.step = 3;
                }
                _ => {
                    if self.ended.is_empty() {
                        return;
                    }
                    assert_eq!(self.ended, [(self.drag.expect("a drag"), DropAction::Move)]);
                    cx.quit();
                }
            }
        }
    }

    pub fn run() {
        // SAFETY: nothing else runs yet.
        unsafe {
            std::env::set_var("SIDESTEP_APPEARANCE", "light");
            std::env::remove_var("WAYLAND_DISPLAY");
        }
        testing::use_null_backend();
        let test = App::new().run(Test::default()).expect("the null render thread");
        assert_eq!(test.ended.len(), 1);
        println!("drag_out: ok");
    }
}
