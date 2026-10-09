//! The toolkit end to end through the null render thread: a window
//! opening at the size it asked for, drawing (the pixels and text the
//! render thread rasterized), invalidating part of it, input, timers, a
//! proxy waking the loop, cursors and maximizing, drag and drop (the type
//! taken as the handler asks), touchpad coasting, hiding and showing a
//! window again, popups, and closing, which ends the application; then a
//! second application in the same process. An application's handler plays
//! its steps in turn; each waits for its condition, polled by a timer,
//! with a deadline.

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
        drag_seen: Vec<Drag>,
        dropped: Option<(Drag, DropResponse)>,
        left: usize,
        /// Where events stood when a step began, to look only at newer
        /// ones.
        mark: usize,
        /// The window's second showing, once hidden and shown again.
        reshown: Option<u32>,
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

        /// The events since the step's mark.
        fn since(&self) -> &[WindowEvent] {
            &self.events[self.mark..]
        }

        fn next(&mut self, step: usize) {
            self.step = step;
            self.mark = self.events.len();
        }

        /// The momentum phases of the coasting scrolls since the mark.
        fn coasting(&self) -> Vec<ScrollPhase> {
            self.since()
                .iter()
                .filter_map(|e| match e {
                    WindowEvent::Scroll { momentum, .. } if *momentum != ScrollPhase::None => Some(*momentum),
                    _ => None,
                })
                .collect()
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
                    // Outputs are what was described, at once: the null
                    // render thread describes none.
                    let asked = Instant::now();
                    assert!(cx.outputs().is_empty());
                    assert!(asked.elapsed() < Duration::from_millis(100), "outputs don't wait");
                    // One application at a time.
                    let hook = std::panic::take_hook();
                    std::panic::set_hook(Box::new(|_| {}));
                    let second =
                        std::thread::spawn(|| std::panic::catch_unwind(|| App::new().run(Nothing).is_ok()).is_err());
                    assert!(second.join().expect("the thread"), "a second application while one runs");
                    std::panic::set_hook(hook);
                    let mut window = cx.window(w).expect("open");
                    assert_eq!(window.size(), Size::new(200.0, 100.0));
                    window.invalidate(Rect::new(100.0, 60.0, 120.0, 80.0));
                    self.next(1);
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
                    self.next(2);
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
                    assert!(self.has(|e| matches!(
                        e,
                        WindowEvent::Scroll { delta: ScrollDelta::Lines(d), momentum: ScrollPhase::None, .. }
                            if d.y == 1.0
                    )));
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
                    self.next(3);
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
                    self.next(4);
                }
                // Requests reach the render thread; the compositor's
                // answers come back as events.
                4 => {
                    let maximized = self.has(|e| matches!(e, WindowEvent::StateChanged(s) if s.maximized));
                    if !maximized || !testing::render_idle() {
                        return;
                    }
                    let log = testing::take_render_log();
                    let id = testing::render_id(w);
                    assert!(log.contains(&Seen::Cursor { window: id, name: "pointer".into() }), "{log:?}");
                    assert!(
                        log.iter().any(|s| matches!(s, Seen::Request { request, .. } if request.contains("Maximize")))
                    );
                    assert_eq!(cx.window(w).expect("open").title(), "Renamed");
                    // Files and text: the handler asks for the text.
                    let mimes = ["text/uri-list", "text/plain;charset=utf-8"];
                    testing::inject_drag_enter(cx, 7, w, Point::new(30.0, 30.0), &mimes, &["file:///tmp/a"]);
                    testing::inject_drop(cx, 7);
                    self.next(5);
                }
                // The drag is offered, and dropped as the type asked for.
                5 => {
                    let Some((drag, response)) = self.dropped.take() else { return };
                    assert_eq!(drag.urls, ["file:///tmp/a"]);
                    assert_eq!(response.action, DropAction::Copy);
                    assert_eq!(response.mime.as_deref(), Some("text/plain;charset=utf-8"));
                    assert!(response.periodic);
                    assert_eq!(self.drag_seen.last().map(|d| d.position), Some(Point::new(30.0, 30.0)));
                    assert_eq!(self.left, 0);
                    // Asked for a type the drag doesn't offer, the
                    // toolkit's choice: the URL list.
                    testing::inject_drag_enter(cx, 8, w, Point::new(5.0, 5.0), &["text/uri-list"], &["file:///tmp/b"]);
                    testing::inject_drop(cx, 8);
                    self.next(6);
                }
                6 => {
                    let Some((_, response)) = self.dropped.take() else { return };
                    assert_eq!(response.mime.as_deref(), Some("text/uri-list"));
                    // A flick: the gesture, then coasting.
                    let at = Point::new(50.0, 50.0);
                    testing::inject_touchpad(cx, w, at, Vec2::new(0.0, 4.0), ScrollPhase::Began, Vec2::ZERO);
                    testing::inject_touchpad(cx, w, at, Vec2::new(0.0, 6.0), ScrollPhase::Changed, Vec2::ZERO);
                    let fast = Vec2::new(0.0, 300.0);
                    testing::inject_touchpad(cx, w, at, Vec2::ZERO, ScrollPhase::Ended, fast);
                    self.next(7);
                }
                // It coasts to a stop: began, changed, ended, slower and
                // slower, all of it toward the flick's way.
                7 => {
                    let phases = self.coasting();
                    if phases.last() != Some(&ScrollPhase::Ended) {
                        return;
                    }
                    assert_eq!(phases.first(), Some(&ScrollPhase::Began), "{phases:?}");
                    assert!(phases.len() >= 3, "{phases:?}");
                    let deltas: Vec<f64> = self
                        .since()
                        .iter()
                        .filter_map(|e| match e {
                            WindowEvent::Scroll {
                                momentum: ScrollPhase::Began | ScrollPhase::Changed, delta, ..
                            } => {
                                let ScrollDelta::Points(d) = delta else { panic!("coasting is in points") };
                                Some(d.y)
                            }
                            _ => None,
                        })
                        .collect();
                    assert!(deltas.iter().all(|d| *d > 0.0), "{deltas:?}");
                    let travelled: f64 = deltas.iter().sum();
                    // 300 pt/s decaying to 12: about a hundred points.
                    assert!((60.0..140.0).contains(&travelled), "{travelled} {deltas:?}");
                    // Another flick, which a click stops at once.
                    let at = Point::new(50.0, 50.0);
                    let fast = Vec2::new(0.0, 2000.0);
                    testing::inject_touchpad(cx, w, at, Vec2::ZERO, ScrollPhase::Ended, fast);
                    self.next(8);
                }
                8 => {
                    let phases = self.coasting();
                    if phases.is_empty() {
                        // The first coasting step comes with a frame or
                        // the fallback; then the click.
                        return;
                    }
                    if !phases.contains(&ScrollPhase::Ended) {
                        testing::inject_button(cx, w, Point::new(50.0, 50.0), PointerButton::Left, true, 1);
                        return;
                    }
                    // Ended right after the click, long before 2000 pt/s
                    // would have slowed.
                    let after = self.since().iter().position(|e| matches!(e, WindowEvent::PointerButton { .. }));
                    let ended = self
                        .since()
                        .iter()
                        .position(|e| matches!(e, WindowEvent::Scroll { momentum: ScrollPhase::Ended, .. }));
                    assert!(ended < after, "the coasting ends before the press is told: {:?}", self.since());
                    let mut window = cx.window(w).expect("open");
                    assert!(window.is_visible());
                    window.set_title("Hidden and back");
                    cx.hide_window(w);
                    self.next(9);
                }
                // Hidden: off the screen, without the keyboard; then shown
                // again as a new showing, configured, drawn whole, focused.
                9 => {
                    if !self.since().contains(&WindowEvent::Focused(false)) {
                        return;
                    }
                    assert!(!cx.window(w).expect("still open").is_visible());
                    assert_eq!(testing::showing_id(cx, w), None);
                    let draws = self.draws.len();
                    cx.show_window(w);
                    let showing = testing::showing_id(cx, w).expect("shown");
                    assert_ne!(showing, testing::render_id(w), "a showing of its own");
                    self.reshown = Some(showing);
                    self.mark = self.events.len();
                    self.draws.truncate(draws);
                    self.next(10);
                }
                10 => {
                    let back = self.since().contains(&WindowEvent::Resized(Size::new(200.0, 100.0)))
                        && self.since().contains(&WindowEvent::Focused(true))
                        && !self.draws.is_empty()
                        && testing::render_idle();
                    if !back {
                        return;
                    }
                    assert_eq!(self.draws.last().map(Vec::as_slice), Some(&[Rect::new(0.0, 0.0, 200.0, 100.0)][..]));
                    let log = testing::take_render_log();
                    let (first, again) = (testing::render_id(w), self.reshown.expect("shown again"));
                    assert!(log.contains(&Seen::Closed { window: first }), "{log:?}");
                    assert!(
                        log.iter().any(|s| matches!(s, Seen::Created { window, .. } if *window == again)),
                        "{log:?}"
                    );
                    // Its cursor came back with it.
                    assert!(log.contains(&Seen::Cursor { window: again, name: "pointer".into() }), "{log:?}");
                    assert_eq!(cx.window(w).expect("open").title(), "Hidden and back");
                    // Input goes to the showing.
                    testing::inject_key(cx, w, 30, "b", "b", Modifiers::NONE, true);
                    let anchor = Rect::new(10.0, 10.0, 60.0, 30.0);
                    let options = WindowOptions::popup(w, anchor, Size::new(100.0, 80.0), true)
                        .popup_position(PopupPosition::BESIDE);
                    self.popup = Some(cx.open_window(options));
                    self.next(11);
                }
                // A popup opens over its parent, and closes when the
                // compositor dismisses it.
                11 => {
                    let popup = self.popup.expect("the popup");
                    if !self.popup_events.contains(&WindowEvent::Resized(Size::new(100.0, 80.0))) {
                        return;
                    }
                    if !self.popup_events.contains(&WindowEvent::PopupDismissed) {
                        testing::inject_popup_done(cx, popup);
                        return;
                    }
                    assert!(self.has(|e| matches!(e, WindowEvent::Key(k) if k.text.as_deref() == Some("b"))));
                    assert!(cx.window(popup).is_none(), "dismissed popups close");
                    assert!(self.draws_popup > 0, "the popup drew");
                    // Another stays open over the window as it closes.
                    let anchor = Rect::new(0.0, 0.0, 10.0, 10.0);
                    self.orphan = Some(cx.open_window(WindowOptions::popup(w, anchor, Size::new(40.0, 40.0), false)));
                    testing::inject_close_request(cx, w);
                    self.next(12);
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

        fn drag_moved(&mut self, _cx: &mut Cx, _window: WindowId, drag: &Drag) -> DropResponse {
            self.drag_seen.push(drag.clone());
            match drag.mimes.len() {
                // Files and text: take the text, and be asked again while
                // the drag waits.
                2 => DropResponse::copy().with_mime("text/plain;charset=utf-8").periodic(),
                // Ask for what isn't offered.
                _ if !drag.urls.is_empty() => DropResponse::copy().with_mime("image/png"),
                _ => DropResponse::REFUSE,
            }
        }

        fn drag_left(&mut self, _cx: &mut Cx, _window: WindowId) {
            self.left += 1;
        }

        fn dropped(&mut self, _cx: &mut Cx, _window: WindowId, drag: &Drag, response: &DropResponse) -> bool {
            self.dropped = Some((drag.clone(), response.clone()));
            true
        }
    }

    /// An application that does nothing (it never gets to run).
    struct Nothing;

    impl Handler for Nothing {
        fn draw(&mut self, _cx: &mut Cx, _window: WindowId, _canvas: &mut Canvas) {}
    }

    /// A second application, after the first stopped: a window shows and
    /// draws, and the application quits.
    #[derive(Default)]
    struct Again {
        drawn: bool,
        resized: bool,
    }

    impl Handler for Again {
        fn launched(&mut self, cx: &mut Cx) {
            cx.open_window(WindowOptions::new("Again").size(50.0, 40.0));
            cx.set_timer(DEADLINE);
        }

        fn window_event(&mut self, _cx: &mut Cx, _window: WindowId, event: WindowEvent) {
            self.resized |= event == WindowEvent::Resized(Size::new(50.0, 40.0));
        }

        fn draw(&mut self, cx: &mut Cx, _window: WindowId, canvas: &mut Canvas) {
            canvas.fill_rect(Rect::new(0.0, 0.0, 10.0, 10.0), RED);
            self.drawn = true;
            cx.quit();
        }

        fn timer(&mut self, _cx: &mut Cx, _timer: TimerId) {
            panic!("the second application never drew");
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
        assert_eq!(test.step, 12, "every step ran");
        assert!(test.has(|e| *e == WindowEvent::CloseRequested));
        // The render thread closes the window after the loop returns.
        let main = test.reshown.expect("shown again");
        let popup = testing::render_id(test.popup.expect("a popup"));
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
            "the popup opened over the window's showing: {log:?}"
        );
        assert!(log.contains(&Seen::Closed { window: popup }), "{log:?}");
        // A popup closes before its parent.
        let orphan = testing::render_id(test.orphan.expect("a second popup"));
        let at = |w| log.iter().position(|s| *s == Seen::Closed { window: w });
        assert!(at(orphan).expect("the second popup closed") < at(main).expect("the window closed"), "{log:?}");
        // Another application after it.
        let again = App::new().run(Again::default()).expect("a second application");
        assert!(again.resized && again.drawn);
        println!("headless: ok");
    }
}
