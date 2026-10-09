//! The toolkit end to end through the null render thread: a window
//! opening at the size it asked for, drawing (the pixels and text the
//! render thread rasterized), invalidating part of it, input, timers, a
//! proxy waking the loop, cursors and maximizing, drag and drop, a popup,
//! and closing, which ends the application. A process runs one
//! application, so one handler plays the steps in turn; each waits for its
//! condition, polled by a timer, with a deadline.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use sidestep_ui::kurbo::{Point, Rect, Size, Vec2};
    use sidestep_ui::testing::{self, Seen};
    use sidestep_ui::*;

    const RED: Color = Color::rgb(1.0, 0.0, 0.0);
    const GROUND: Color = Color::rgb(0.0, 0.0, 1.0);
    const DEADLINE: Duration = Duration::from_secs(30);

    #[derive(Default)]
    struct Test {
        step: usize,
        started: Option<Instant>,
        window: Option<WindowId>,
        poll: Option<TimerId>,
        events: Vec<WindowEvent>,
        draws: Vec<Vec<Rect>>,
        once: Option<TimerId>,
        repeating: Option<TimerId>,
        cancelled: Option<TimerId>,
        fired: Vec<TimerId>,
        woken: Arc<AtomicBool>,
        handler_woken: bool,
        drag_seen: Option<Drag>,
        dropped: Option<(Drag, DropAction)>,
        left: usize,
        popup: Option<WindowId>,
        popup_events: Vec<WindowEvent>,
        draws_popup: usize,
        orphan: Option<WindowId>,
    }

    impl Test {
        fn window(&self) -> WindowId {
            self.window.expect("the window")
        }

        fn has(&self, f: impl Fn(&WindowEvent) -> bool) -> bool {
            self.events.iter().any(f)
        }

        /// Take the next step if this one's condition holds.
        fn advance(&mut self, cx: &mut Cx) {
            let w = self.window();
            match self.step {
                // Opened, configured, drawn and shown.
                0 => {
                    let shown = self.has(|e| *e == WindowEvent::Resized(Size::new(200.0, 100.0)))
                        && self.has(|e| *e == WindowEvent::ScaleChanged(1.0))
                        && !self.draws.is_empty()
                        && testing::render_idle();
                    if !shown {
                        return;
                    }
                    assert_eq!(self.draws[0], [Rect::new(0.0, 0.0, 200.0, 100.0)], "the first pass draws it all");
                    let (width, height, px) = testing::window_pixels(w).expect("captured pixels");
                    assert_eq!((width, height), (200, 100));
                    let at = |x: usize, y: usize| px[y * 200 + x];
                    assert_eq!(at(20, 20), [255, 0, 0, 255], "the red square");
                    assert_eq!(at(5, 5), [0, 0, 255, 255], "the window's background");
                    let text = testing::take_painted_text();
                    assert!(
                        text.iter().any(|t| t.window == testing::render_id(w) && (t.x - 60.0).abs() < 1.0),
                        "the label's glyphs: {text:?}"
                    );
                    let mut window = cx.window(w).expect("open");
                    assert_eq!(window.size(), Size::new(200.0, 100.0));
                    window.invalidate(Rect::new(100.0, 60.0, 120.0, 80.0));
                    self.step = 1;
                }
                // Only what was invalidated is drawn again.
                1 => {
                    if self.draws.len() < 2 {
                        return;
                    }
                    assert_eq!(self.draws[1], [Rect::new(100.0, 60.0, 120.0, 80.0)]);
                    testing::inject_key(cx, w, 30, "a", "a", Modifiers::NONE, true);
                    testing::inject_key(cx, w, 30, "\u{1}", "a", Modifiers::CONTROL, true);
                    testing::inject_key(cx, w, 103, "\u{F700}", "\u{F700}", Modifiers::NONE, true);
                    testing::inject_button(cx, w, Point::new(20.0, 20.0), PointerButton::Left, true, 2);
                    testing::inject_motion(cx, w, Point::new(30.0, 25.0));
                    testing::inject_motion(cx, w, Point::new(31.0, 26.0));
                    testing::inject_wheel(cx, w, Point::new(31.0, 26.0), Vec2::new(0.0, 1.0));
                    testing::inject_ime(cx, w, Some("é"), "");
                    self.step = 2;
                }
                // Input arrives translated.
                2 => {
                    if !self.has(|e| matches!(e, WindowEvent::Ime(_))) {
                        return;
                    }
                    let keys: Vec<&KeyEvent> = self
                        .events
                        .iter()
                        .filter_map(|e| if let WindowEvent::Key(k) = e { Some(k) } else { None })
                        .collect();
                    assert_eq!(keys.len(), 3, "{keys:?}");
                    assert_eq!(
                        (&keys[0].key, keys[0].text.as_deref(), keys[0].code),
                        (&Key::Character("a".into()), Some("a"), 30)
                    );
                    assert_eq!((&keys[1].key, keys[1].text.as_deref()), (&Key::Character("a".into()), None));
                    assert!(keys[1].modifiers.control());
                    assert_eq!(keys[2].key, Key::Named(NamedKey::ArrowUp));
                    assert!(self.has(|e| matches!(
                        e,
                        WindowEvent::PointerButton { button: PointerButton::Left, pressed: true, clicks: 2, .. }
                    )));
                    // Moves one after another come as the last of them.
                    let moves: Vec<Point> = self
                        .events
                        .iter()
                        .filter_map(|e| {
                            if let WindowEvent::PointerMoved { position, .. } = e { Some(*position) } else { None }
                        })
                        .collect();
                    assert_eq!(moves, [Point::new(31.0, 26.0)]);
                    assert!(
                        self.has(
                            |e| matches!(e, WindowEvent::Scroll { delta: ScrollDelta::Lines(d), .. } if d.y == 1.0)
                        )
                    );
                    assert!(self.has(|e| *e == WindowEvent::Ime(Ime::Commit("é".into()))));
                    self.once = Some(cx.set_timer(Duration::from_millis(1)));
                    self.repeating = Some(cx.set_repeating_timer(Duration::from_millis(1)));
                    let cancelled = cx.set_timer(Duration::from_millis(1));
                    cx.cancel_timer(cancelled);
                    self.cancelled = Some(cancelled);
                    let proxy = cx.proxy();
                    let woken = self.woken.clone();
                    std::thread::spawn(move || {
                        woken.store(true, Ordering::Release);
                        proxy.wake();
                    });
                    self.step = 3;
                }
                // Timers come due; cancelled ones don't. A proxy wakes the
                // loop from another thread.
                3 => {
                    let repeats = self.fired.iter().filter(|t| Some(**t) == self.repeating).count();
                    if repeats < 3 || !self.handler_woken {
                        return;
                    }
                    assert_eq!(self.fired.iter().filter(|t| Some(**t) == self.once).count(), 1);
                    assert!(!self.fired.contains(&self.cancelled.expect("a timer")));
                    cx.cancel_timer(self.repeating.expect("a timer"));
                    let mut window = cx.window(w).expect("open");
                    window.set_cursor(Cursor::Pointer);
                    window.set_title("Renamed");
                    window.set_maximized(true);
                    self.step = 4;
                }
                // Requests reach the render thread; the compositor's
                // answers come back as events.
                4 => {
                    let maximized = self.has(|e| matches!(e, WindowEvent::StateChanged(s) if s.maximized));
                    if !maximized || !testing::render_idle() {
                        return;
                    }
                    let log = testing::take_render_log();
                    assert!(
                        log.contains(&Seen::Cursor { window: testing::render_id(w), name: "pointer".into() }),
                        "{log:?}"
                    );
                    assert!(
                        log.iter().any(|s| matches!(s, Seen::Request { request, .. } if request.contains("Maximize")))
                    );
                    assert_eq!(cx.window(w).expect("open").title(), "Renamed");
                    testing::inject_drag_enter(
                        cx,
                        7,
                        w,
                        Point::new(30.0, 30.0),
                        &["text/uri-list"],
                        &["file:///tmp/a"],
                    );
                    testing::inject_drop(cx, 7);
                    self.step = 5;
                }
                // A drag of files is offered, and dropped.
                5 => {
                    let Some((drag, action)) = &self.dropped else { return };
                    assert_eq!(drag.urls, ["file:///tmp/a"]);
                    assert_eq!(*action, DropAction::Copy);
                    assert_eq!(self.drag_seen.as_ref().map(|d| d.position), Some(Point::new(30.0, 30.0)));
                    assert_eq!(self.left, 0);
                    let anchor = Rect::new(10.0, 10.0, 60.0, 30.0);
                    self.popup = Some(cx.open_window(WindowOptions::popup(w, anchor, Size::new(100.0, 80.0), true)));
                    self.step = 6;
                }
                // A popup opens over its parent, and closes when the
                // compositor dismisses it.
                6 => {
                    let popup = self.popup.expect("the popup");
                    if !self.popup_events.contains(&WindowEvent::Resized(Size::new(100.0, 80.0))) {
                        return;
                    }
                    if !self.popup_events.contains(&WindowEvent::PopupDismissed) {
                        testing::inject_popup_done(cx, popup);
                        return;
                    }
                    assert!(cx.window(popup).is_none(), "dismissed popups close");
                    assert!(self.draws_popup > 0, "the popup drew");
                    // Another stays open over the window as it closes.
                    let anchor = Rect::new(0.0, 0.0, 10.0, 10.0);
                    self.orphan = Some(cx.open_window(WindowOptions::popup(w, anchor, Size::new(40.0, 40.0), false)));
                    testing::inject_close_request(cx, w);
                    self.step = 7;
                }
                // Closing the last window ends the application.
                _ => {}
            }
        }
    }

    impl Handler for Test {
        fn launched(&mut self, cx: &mut Cx) {
            self.started = Some(Instant::now());
            let options = WindowOptions::new("Headless").size(200.0, 100.0).background(Background::Color(GROUND));
            self.window = Some(cx.open_window(options));
            self.poll = Some(cx.set_repeating_timer(Duration::from_millis(2)));
        }

        fn window_event(&mut self, _cx: &mut Cx, window: WindowId, event: WindowEvent) {
            if Some(window) == self.popup || Some(window) == self.orphan {
                self.popup_events.push(event);
                return;
            }
            assert_eq!(window, self.window());
            self.events.push(event);
        }

        fn draw(&mut self, _cx: &mut Cx, window: WindowId, canvas: &mut Canvas) {
            if Some(window) == self.popup || Some(window) == self.orphan {
                self.draws_popup += 1;
                return;
            }
            self.draws.push(canvas.damage());
            canvas.fill_rect(Rect::new(10.0, 10.0, 50.0, 50.0), RED);
            let style = TextStyle::new(Font::system(13.0), Color::BLACK);
            canvas.draw_label("Sidestep", &style, Point::new(60.0, 10.0));
        }

        fn timer(&mut self, cx: &mut Cx, timer: TimerId) {
            if Some(timer) != self.poll {
                self.fired.push(timer);
                return;
            }
            let started = self.started.expect("launched");
            assert!(started.elapsed() < DEADLINE, "step {} didn't finish in {DEADLINE:?}", self.step);
            self.advance(cx);
        }

        fn woken(&mut self, _cx: &mut Cx) {
            assert!(self.woken.load(Ordering::Acquire));
            self.handler_woken = true;
        }

        fn drag_moved(&mut self, _cx: &mut Cx, _window: WindowId, drag: &Drag) -> DropAction {
            self.drag_seen = Some(drag.clone());
            if drag.urls.is_empty() { DropAction::None } else { DropAction::Copy }
        }

        fn drag_left(&mut self, _cx: &mut Cx, _window: WindowId) {
            self.left += 1;
        }

        fn dropped(&mut self, _cx: &mut Cx, _window: WindowId, drag: &Drag, action: DropAction) -> bool {
            self.dropped = Some((drag.clone(), action));
            true
        }
    }

    pub fn run() {
        // Neither the desktop's appearance nor its display may matter.
        // SAFETY: nothing else runs yet.
        unsafe {
            std::env::set_var("SIDESTEP_APPEARANCE", "light");
            std::env::set_var("SIDESTEP_ACCENT", "#3584e4");
            std::env::remove_var("WAYLAND_DISPLAY");
        }
        testing::use_null_backend();
        testing::capture_pixels(true);
        testing::note_painted_text(true);
        let test = App::new().run(Test::default()).expect("the null render thread runs");
        assert_eq!(test.step, 7, "every step ran");
        assert!(test.has(|e| *e == WindowEvent::CloseRequested));
        // The render thread closes the window after the loop returns.
        let (main, popup) = (testing::render_id(test.window()), testing::render_id(test.popup.expect("a popup")));
        let until = Instant::now() + DEADLINE;
        let mut log = Vec::new();
        while !log.contains(&Seen::Closed { window: main }) {
            assert!(Instant::now() < until, "the window never closed: {log:?}");
            log.extend(testing::take_render_log());
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            log.iter().any(
                |s| matches!(s, Seen::Created { window, popup_of: Some(p), .. } if *window == popup && *p == main)
            ),
            "{log:?}"
        );
        assert!(log.contains(&Seen::Closed { window: popup }), "{log:?}");
        // A popup closes before its parent.
        let orphan = testing::render_id(test.orphan.expect("a second popup"));
        let at = |w| log.iter().position(|s| *s == Seen::Closed { window: w });
        assert!(at(orphan).expect("the second popup closed") < at(main).expect("the window closed"), "{log:?}");
        println!("headless: ok");
    }
}
