//! The toolkit under a real compositor, with other clients: a virtual
//! pointer (a client of the test's own, on a thread) moving, clicking,
//! double-clicking and turning the wheel over the window and dismissing a
//! popup; `wtype` typing; `wl-copy` and `wl-paste` on the other side of the
//! clipboard; the compositor's outputs; and the window hidden and shown
//! again, still taking input. Linux only, and only under the headless sway
//! (a virtual pointer on a desktop would move its pointer):
//!
//! ```sh
//! scripts/linux-run scripts/headless-wayland cargo test -p sidestep-ui --test wayland_system
//! ```
//!
//! Elsewhere it says so and passes. Without `wtype` or the clipboard
//! tools, it leaves those steps out.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use sidestep_ui::kurbo::{Point, Rect, Size};
    use sidestep_ui::*;

    const DEADLINE: Duration = Duration::from_secs(60);

    fn has(tool: &str) -> bool {
        Command::new("sh").arg("-c").arg(format!("command -v {tool}")).output().is_ok_and(|o| o.status.success())
    }

    /// Run a shell command on a thread of its own, its standard output
    /// coming back on the channel.
    fn spawn_sh(command: &str) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        let command = command.to_owned();
        std::thread::spawn(move || {
            let out = Command::new("sh").arg("-c").arg(&command).stderr(Stdio::inherit()).output();
            let _ = tx.send(out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default());
        });
        rx
    }

    #[derive(PartialEq)]
    enum Step {
        Showing,
        Clicked,
        Wheeled,
        Typed,
        Pasted,
        Copied,
        Floated,
        Popup,
        Reshown,
        Done,
    }

    struct Test {
        step: Step,
        started: Instant,
        window: Option<WindowId>,
        poll: Option<TimerId>,
        events: Vec<WindowEvent>,
        /// Where events stood when the step began.
        mark: usize,
        draws: usize,
        pointer: mpsc::Sender<pointer::Do>,
        typing: bool,
        clipboard: bool,
        pasted: Option<mpsc::Receiver<String>>,
        popup: Option<WindowId>,
        popup_events: Vec<WindowEvent>,
        /// The step sent its input (it waits for what comes of it).
        asked: bool,
    }

    impl Test {
        fn window(&self) -> WindowId {
            self.window.expect("the window")
        }

        fn since(&self) -> &[WindowEvent] {
            &self.events[self.mark..]
        }

        fn next(&mut self, step: Step) {
            self.step = step;
            self.mark = self.events.len();
            self.asked = false;
        }

        fn point(&self, to: pointer::Do) {
            self.pointer.send(to).expect("the virtual pointer");
        }

        /// Take the next step if this one's condition holds.
        fn advance(&mut self, cx: &mut Cx) {
            let w = self.window();
            match self.step {
                // Tiled across the output, drawn, and the outputs described.
                Step::Showing => {
                    let size = cx.window(w).map(|w| w.size());
                    if self.draws == 0 || cx.outputs().is_empty() || size.is_none_or(|s| s.width < 640.0) {
                        return;
                    }
                    let outputs = cx.outputs();
                    println!("  outputs: {outputs:?}");
                    let output = &outputs[0];
                    assert_eq!(output.frame.origin(), Point::ZERO);
                    assert_eq!(output.frame.size(), size.expect("a size"), "sway tiles the window over the output");
                    assert!(output.scale >= 1.0);
                    self.point(pointer::Do::MoveTo(200, 150));
                    self.point(pointer::Do::Click);
                    self.point(pointer::Do::Click);
                    self.next(Step::Clicked);
                }
                // A click and a double click where the pointer is.
                Step::Clicked => {
                    let presses: Vec<(Point, u32)> = self
                        .since()
                        .iter()
                        .filter_map(|e| match e {
                            WindowEvent::PointerButton {
                                position,
                                button: PointerButton::Left,
                                pressed: true,
                                clicks,
                                ..
                            } => Some((*position, *clicks)),
                            _ => None,
                        })
                        .collect();
                    let released = self
                        .since()
                        .iter()
                        .filter(|e| matches!(e, WindowEvent::PointerButton { pressed: false, .. }))
                        .count();
                    if presses.len() < 2 || released < 2 {
                        return;
                    }
                    println!("  presses: {presses:?}");
                    for (at, _) in &presses {
                        assert!(at.distance(Point::new(200.0, 150.0)) < 1.0, "{presses:?}");
                    }
                    assert_eq!((presses[0].1, presses[1].1), (1, 2), "a click, then a double click");
                    assert!(self.events.iter().any(|e| matches!(e, WindowEvent::PointerEntered(_))));
                    self.point(pointer::Do::MoveTo(400, 300));
                    self.point(pointer::Do::Wheel(1));
                    self.next(Step::Wheeled);
                }
                // The wheel, a detent down, where the pointer moved.
                Step::Wheeled => {
                    let Some((at, delta)) = self.since().iter().find_map(|e| match e {
                        WindowEvent::Scroll { position, delta: ScrollDelta::Lines(d), .. } => Some((*position, *d)),
                        _ => None,
                    }) else {
                        return;
                    };
                    println!("  wheel: {delta:?} at {at:?}");
                    assert!(at.distance(Point::new(400.0, 300.0)) < 1.0);
                    assert!((delta.y - 1.0).abs() < 0.01 && delta.x == 0.0, "{delta:?}");
                    if self.typing {
                        // wtype's first press goes while the compositor
                        // takes its keymap.
                        let _ = spawn_sh("wtype -k Shift_L 'yz'");
                        self.next(Step::Typed);
                    } else {
                        println!("  (no wtype: no typing)");
                        self.next(Step::Copied);
                    }
                }
                // Keys typed through the compositor's keymap.
                Step::Typed => {
                    let typed: String = self
                        .since()
                        .iter()
                        .filter_map(|e| if let WindowEvent::Key(k) = e { k.text.clone() } else { None })
                        .collect();
                    if typed != "yz" {
                        assert!("yz".starts_with(&typed), "{typed:?}");
                        return;
                    }
                    if self.clipboard {
                        cx.set_clipboard_text("from sidestep-ui");
                        self.pasted = Some(spawn_sh("sleep 0.3; timeout 5 wl-paste --no-newline"));
                        self.next(Step::Pasted);
                    } else {
                        println!("  (no wl-clipboard: no clipboard)");
                        self.next(Step::Copied);
                    }
                }
                // Another client reads what the program copied, and the
                // program reads what another copied.
                Step::Pasted => {
                    let Some(Ok(pasted)) = self.pasted.as_ref().map(mpsc::Receiver::try_recv) else { return };
                    assert_eq!(pasted, "from sidestep-ui");
                    println!("  wl-paste read: {pasted:?}");
                    let _ = spawn_sh("printf 'from outside' | wl-copy");
                    self.next(Step::Copied);
                }
                Step::Copied => {
                    if self.clipboard && self.typing {
                        let text = cx.clipboard_text();
                        if text.as_deref() != Some("from outside") {
                            return;
                        }
                        println!("  read wl-copy's: {text:?}");
                    }
                    if !has("swaymsg") {
                        println!("  (no swaymsg: no popup)");
                        cx.hide_window(w);
                        cx.show_window(w);
                        self.draws = 0;
                        self.next(Step::Reshown);
                        return;
                    }
                    // A click on the program's own window goes to it while
                    // its popup has the grab: the window floats in a corner,
                    // leaving the rest of the output to click on.
                    let _ = spawn_sh(
                        "swaymsg -s \"$(ls \"$XDG_RUNTIME_DIR\"/sway-ipc.*.sock | head -n1)\" -- \
                         '[title=\"Wayland system\"] floating enable, resize set 600 400, move position 0 0' >/dev/null",
                    );
                    self.next(Step::Floated);
                }
                Step::Floated => {
                    if !self.since().contains(&WindowEvent::Resized(Size::new(600.0, 400.0))) {
                        return;
                    }
                    // A press opens the popup, as menus open: its serial
                    // lets the popup take the grab.
                    self.point(pointer::Do::MoveTo(120, 110));
                    self.point(pointer::Do::Click);
                    self.next(Step::Popup);
                }
                // A grabbing popup below its anchor, dismissed by a click
                // elsewhere.
                Step::Popup => {
                    let Some(popup) = self.popup else { return };
                    if !self.popup_events.iter().any(|e| matches!(e, WindowEvent::Resized(_))) {
                        return;
                    }
                    if !self.popup_events.contains(&WindowEvent::PopupDismissed) {
                        if !std::mem::replace(&mut self.asked, true) {
                            // A click outside, once it has shown.
                            self.point(pointer::Do::MoveTo(1000, 700));
                            self.point(pointer::Do::Click);
                        }
                        return;
                    }
                    assert!(cx.window(popup).is_none());
                    println!("  popup dismissed by a click outside");
                    cx.hide_window(w);
                    cx.show_window(w);
                    self.draws = 0;
                    self.next(Step::Reshown);
                }
                // Hidden and shown again: configured, drawn, and taking
                // clicks at its new showing.
                Step::Reshown => {
                    if self.draws == 0 {
                        return;
                    }
                    let clicked =
                        self.since().iter().any(|e| matches!(e, WindowEvent::PointerButton { pressed: true, .. }));
                    if !clicked {
                        if !std::mem::replace(&mut self.asked, true) {
                            self.point(pointer::Do::MoveTo(300, 200));
                            self.point(pointer::Do::Click);
                        }
                        return;
                    }
                    println!("  shown again, and clicked");
                    self.point(pointer::Do::Done);
                    self.next(Step::Done);
                    cx.quit();
                }
                Step::Done => {}
            }
        }
    }

    impl Handler for Test {
        fn launched(&mut self, cx: &mut Cx) {
            self.window = Some(cx.open_window(WindowOptions::new("Wayland system").size(300.0, 200.0)));
            self.poll = Some(cx.set_repeating_timer(Duration::from_millis(10)));
        }

        fn window_event(&mut self, cx: &mut Cx, window: WindowId, event: WindowEvent) {
            if Some(window) == self.popup {
                self.popup_events.push(event);
                return;
            }
            // The press in the window opens the popup, at once.
            if self.step == Step::Popup
                && self.popup.is_none()
                && matches!(event, WindowEvent::PointerButton { pressed: true, .. })
            {
                let anchor = Rect::new(100.0, 100.0, 150.0, 120.0);
                self.popup = Some(cx.open_window(WindowOptions::popup(window, anchor, Size::new(120.0, 60.0), true)));
            }
            self.events.push(event);
        }

        fn draw(&mut self, _cx: &mut Cx, window: WindowId, canvas: &mut Canvas) {
            let color = if Some(window) == self.popup { Color::hex(0x3584e4) } else { Color::hex(0x26a269) };
            canvas.fill_rect(Rect::from_origin_size(Point::ZERO, canvas.size()), color);
            if Some(window) != self.popup {
                self.draws += 1;
            }
        }

        fn timer(&mut self, cx: &mut Cx, timer: TimerId) {
            if Some(timer) == self.poll {
                assert!(self.started.elapsed() < DEADLINE, "stuck after {:?}", self.events);
                self.advance(cx);
            }
        }
    }

    pub fn run() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var("WLR_BACKENDS").as_deref() != Ok("headless") {
            println!("skipped: needs the headless sway (run it under scripts/headless-wayland)");
            return;
        }
        // SAFETY: nothing else runs yet.
        unsafe { std::env::set_var("SIDESTEP_APPEARANCE", "light") };
        let (tx, rx) = mpsc::channel();
        let pointing = std::thread::spawn(move || pointer::run(rx));
        let test = Test {
            step: Step::Showing,
            started: Instant::now(),
            window: None,
            poll: None,
            events: Vec::new(),
            mark: 0,
            draws: 0,
            pointer: tx,
            typing: has("wtype"),
            clipboard: has("wl-copy") && has("wl-paste"),
            pasted: None,
            popup: None,
            popup_events: Vec::new(),
            asked: false,
        };
        let test = App::new().run(test).expect("a compositor");
        assert!(test.step == Step::Done, "every step ran");
        drop(test);
        pointing.join().expect("the virtual pointer");
        println!("wayland_system: ok");
    }

    /// A virtual pointer: a client of its own, moving and clicking where
    /// it's told, in the output's points (the headless sway's one output,
    /// 1280 × 800 unless `SIZE` says otherwise, at scale 1).
    mod pointer {
        use std::sync::mpsc::Receiver;

        use smithay_client_toolkit::reexports::client::globals::{GlobalListContents, registry_queue_init};
        use smithay_client_toolkit::reexports::client::protocol::wl_pointer::{Axis, AxisSource, ButtonState};
        use smithay_client_toolkit::reexports::client::protocol::wl_registry::WlRegistry;
        use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
        use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
        use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
        use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

        pub enum Do {
            MoveTo(u32, u32),
            Click,
            /// Detents down (up if negative).
            Wheel(i32),
            Done,
        }

        const BTN_LEFT: u32 = 0x110;

        struct State;

        pub fn run(commands: Receiver<Do>) {
            let conn = Connection::connect_to_env().expect("a compositor");
            let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("its registry");
            let qh = queue.handle();
            let seat: WlSeat = globals.bind(&qh, 1..=7, ()).expect("a seat");
            let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).expect("virtual pointers");
            let pointer = manager.create_virtual_pointer(Some(&seat), &qh, ());
            queue.roundtrip(&mut State).expect("a round trip");
            let (width, height) = std::env::var("SIZE")
                .ok()
                .and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
                .unwrap_or((1280, 800));
            let time =
                || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u32;
            for command in commands {
                match command {
                    Do::MoveTo(x, y) => {
                        pointer.motion_absolute(time(), x, y, width, height);
                        pointer.frame();
                    }
                    Do::Click => {
                        pointer.button(time(), BTN_LEFT, ButtonState::Pressed);
                        pointer.frame();
                        pointer.button(time(), BTN_LEFT, ButtonState::Released);
                        pointer.frame();
                    }
                    Do::Wheel(detents) => {
                        pointer.axis_source(AxisSource::Wheel);
                        pointer.axis_discrete(time(), Axis::VerticalScroll, 15.0 * f64::from(detents), detents);
                        pointer.frame();
                    }
                    Do::Done => break,
                }
                conn.flush().expect("sent");
                // Clicks of a double click come well inside its 400 ms.
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            pointer.destroy();
            let _ = conn.flush();
        }

        impl Dispatch<WlRegistry, GlobalListContents> for State {
            fn event(
                _: &mut Self,
                _: &WlRegistry,
                _: <WlRegistry as Proxy>::Event,
                _: &GlobalListContents,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<WlSeat, ()> for State {
            fn event(
                _: &mut Self,
                _: &WlSeat,
                _: <WlSeat as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for State {
            fn event(
                _: &mut Self,
                _: &ZwlrVirtualPointerManagerV1,
                _: <ZwlrVirtualPointerManagerV1 as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<ZwlrVirtualPointerV1, ()> for State {
            fn event(
                _: &mut Self,
                _: &ZwlrVirtualPointerV1,
                _: <ZwlrVirtualPointerV1 as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    }
}
