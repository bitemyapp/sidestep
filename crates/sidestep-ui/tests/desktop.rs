//! A desktop environment's surfaces through the null render thread:
//! layer surfaces (a panel along the top, a wallpaper the compositor
//! sizes, one that takes the keyboard), sized, drawn and taking input as
//! windows do, placed anew while they show and shown again as they were
//! left, and a popup opened from the panel. Each step waits for its
//! condition, polled by a timer, with a deadline.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use sidestep_ui::kurbo::{Point, Rect, Size};
    use sidestep_ui::layer_shell::{Anchors, KeyboardInteractivity, Layer, LayerOptions, Margins};
    use sidestep_ui::testing::{self, Seen};
    use sidestep_ui::*;

    const DEADLINE: Duration = Duration::from_secs(30);
    const BAR: Color = Color::rgb8(0x1d, 0x2b, 0x53);
    /// The null render thread's output.
    const OUTPUT: Size = Size::new(1280.0, 800.0);

    #[derive(Default)]
    struct Test {
        step: usize,
        started: Option<Instant>,
        bar: Option<WindowId>,
        wallpaper: Option<WindowId>,
        search: Option<WindowId>,
        menu: Option<WindowId>,
        events: HashMap<WindowId, Vec<WindowEvent>>,
        draws: HashMap<WindowId, usize>,
        log: Vec<Seen>,
    }

    impl Test {
        fn bar(&self) -> WindowId {
            self.bar.expect("the bar")
        }

        fn saw(&self, window: Option<WindowId>, event: &WindowEvent) -> bool {
            window.and_then(|w| self.events.get(&w)).is_some_and(|e| e.contains(event))
        }

        fn drawn(&self, window: Option<WindowId>) -> bool {
            window.and_then(|w| self.draws.get(&w)).is_some_and(|n| *n > 0)
        }

        /// The placement the render thread was given for `window`'s
        /// showing `showing`, the latest.
        fn placed(&self, showing: u32) -> Option<&testing::LayerPlacement> {
            self.log.iter().rev().find_map(|s| match s {
                Seen::Layer { window, placement } if *window == showing => Some(placement),
                _ => None,
            })
        }

        fn requests(&self, showing: u32) -> Vec<&str> {
            self.log
                .iter()
                .filter_map(|s| match s {
                    Seen::Request { window, request } if *window == showing => Some(request.as_str()),
                    _ => None,
                })
                .collect()
        }

        fn advance(&mut self, cx: &mut Cx) {
            self.log.extend(testing::take_render_log());
            let bar = self.bar();
            let showing = testing::showing_id(cx, bar).unwrap_or(0);
            match self.step {
                // The panel along the top: as wide as the output, as tall
                // as it asked, placed as asked, drawn, and without the
                // keyboard; the wallpaper the output's size.
                0 => {
                    let wide = WindowEvent::Resized(Size::new(OUTPUT.width, 24.0));
                    let whole = WindowEvent::Resized(OUTPUT);
                    if !self.saw(self.bar, &wide)
                        || !self.saw(self.wallpaper, &whole)
                        || !self.drawn(self.bar)
                        || !testing::render_idle()
                    {
                        return;
                    }
                    let placement = self.placed(showing).expect("placed as a layer surface");
                    assert_eq!(placement.namespace, "panel");
                    assert_eq!(placement.exclusive_zone, 24);
                    assert!(placement.output.is_none());
                    assert!(!self.saw(self.bar, &WindowEvent::Focused(true)), "a panel never has the keyboard");
                    let (w, h, px) = testing::window_pixels(bar).expect("the bar's pixels");
                    assert_eq!((w, h), (1280, 24));
                    let p = px[5 * 1280 + 5];
                    assert_eq!(p, [0x1d, 0x2b, 0x53, 0xff], "drawn");
                    assert_eq!(cx.window(bar).map(|w| w.size()), Some(Size::new(OUTPUT.width, 24.0)));
                    // Input reaches it as any window.
                    testing::inject_button(cx, bar, Point::new(40.0, 12.0), PointerButton::Left, true, 1);
                    testing::inject_key(cx, bar, 30, "a", "a", Modifiers::NONE, true);
                    self.step = 1;
                }
                // A press opens a menu from the panel.
                1 => {
                    let events = &self.events[&bar];
                    let pressed = events.iter().any(|e| matches!(e, WindowEvent::PointerButton { pressed: true, .. }));
                    let typed =
                        events.iter().any(|e| matches!(e, WindowEvent::Key(k) if k.text.as_deref() == Some("a")));
                    if !pressed || !typed {
                        return;
                    }
                    let anchor = Rect::new(30.0, 0.0, 90.0, 24.0);
                    self.menu = Some(cx.open_window(WindowOptions::popup(bar, anchor, Size::new(160.0, 120.0), true)));
                    self.step = 2;
                }
                2 => {
                    let menu = self.menu.and_then(|m| testing::showing_id(cx, m));
                    let opened = self.log.iter().any(|s| {
                        matches!(s, Seen::Created { window, popup_of: Some(p), .. } if Some(*window) == menu && *p == showing)
                    });
                    if !opened || !self.drawn(self.menu) {
                        return;
                    }
                    cx.close_window(self.menu.take().expect("the menu"));
                    // Placed anew while it shows: taller, kept off the
                    // output's sides, on the overlay layer, with a new zone,
                    // taking the keyboard.
                    let mut w = cx.window(bar).expect("the bar");
                    w.set_layer_size(Size::new(0.0, 30.0));
                    w.set_layer_margins(Margins::new(0, 10, 0, 10));
                    w.set_layer(Layer::Overlay);
                    w.set_layer_exclusive_zone(30);
                    w.set_layer_keyboard(KeyboardInteractivity::Exclusive);
                    self.step = 3;
                }
                3 => {
                    if !self.saw(self.bar, &WindowEvent::Resized(Size::new(1260.0, 30.0)))
                        || !self.saw(self.bar, &WindowEvent::Focused(true))
                    {
                        return;
                    }
                    let asked = self.requests(showing);
                    for r in
                        ["Resize(0, 30)", "LayerMargins([0, 10, 0, 10])", "Layer(Overlay)", "LayerExclusiveZone(30)"]
                    {
                        assert!(asked.contains(&r), "{r} in {asked:?}");
                    }
                    let options = cx.window(bar).and_then(|w| w.layer().cloned()).expect("a layer surface");
                    assert_eq!((options.layer, options.exclusive_zone), (Layer::Overlay, 30));
                    // Shown again, it's placed as it was left.
                    cx.hide_window(bar);
                    cx.show_window(bar);
                    self.step = 4;
                }
                4 => {
                    let Some(placement) = self.placed(showing) else { return };
                    if !self.saw(self.bar, &WindowEvent::Resized(Size::new(1260.0, 30.0))) {
                        return;
                    }
                    assert_eq!(placement.margins, [0, 10, 0, 10]);
                    assert_eq!(placement.exclusive_zone, 30);
                    // A layer surface that takes the keyboard has it at once.
                    let options = LayerOptions::new(Layer::Top, "search")
                        .anchors(Anchors::TOP)
                        .margins(Margins::new(100, 0, 0, 0))
                        .keyboard(KeyboardInteractivity::Exclusive);
                    self.search = Some(cx.open_window(WindowOptions::layer(options).size(400.0, 40.0)));
                    self.step = 5;
                }
                5 => {
                    if !self.saw(self.search, &WindowEvent::Focused(true))
                        || !self.saw(self.search, &WindowEvent::Resized(Size::new(400.0, 40.0)))
                    {
                        return;
                    }
                    self.step = 6;
                    cx.quit();
                }
                _ => {}
            }
        }
    }

    impl Handler for Test {
        fn launched(&mut self, cx: &mut Cx) {
            self.started = Some(Instant::now());
            let panel = LayerOptions::new(Layer::Top, "panel")
                .anchors(Anchors::TOP | Anchors::LEFT | Anchors::RIGHT)
                .exclusive_zone(24);
            let options = WindowOptions::layer(panel).size(0.0, 24.0).background(Background::Color(BAR));
            self.bar = Some(cx.open_window(options));
            let wallpaper = LayerOptions::new(Layer::Background, "wallpaper").anchors(Anchors::ALL).exclusive_zone(-1);
            self.wallpaper = Some(cx.open_window(WindowOptions::layer(wallpaper)));
            cx.set_repeating_timer(Duration::from_millis(2));
        }

        fn window_event(&mut self, _cx: &mut Cx, window: WindowId, event: WindowEvent) {
            self.events.entry(window).or_default().push(event);
        }

        fn draw(&mut self, _cx: &mut Cx, window: WindowId, _canvas: &mut Canvas) {
            *self.draws.entry(window).or_default() += 1;
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            let started = self.started.expect("launched");
            assert!(started.elapsed() < DEADLINE, "step {} didn't finish in {DEADLINE:?}", self.step);
            self.advance(cx);
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
        let test = App::new().run(Test::default()).expect("the null render thread runs");
        assert_eq!(test.step, 6, "every step ran");
        println!("desktop: ok");
    }
}
