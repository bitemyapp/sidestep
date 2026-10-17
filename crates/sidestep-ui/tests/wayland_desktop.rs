//! A desktop environment's surfaces under a real compositor, the headless
//! sway, with screenshots (`grim`) and a virtual pointer and keyboard of
//! other clients:
//!
//! - a toplevel names its program (its app id, as sway lists it) and is
//!   decorated by the compositor, which sway prefers, drawing nothing of
//!   its own (it shows its content from its top left);
//! - layer surfaces: a panel along the top that windows keep clear of
//!   (sway tiles a window under it), a wallpaper sway sizes to the
//!   output, both showing where they should; a click on the panel opening
//!   a menu from it, which closes; the panel taking the
//!   keyboard when asked, typed into, then growing, and windows making
//!   room; a window's frames measured; the panel taking clicks only in
//!   its input region, the wallpaper under it the others;
//! - outputs as sway names and places them; one plugged in (heard of), a
//!   panel put on it, and the output unplugged, which closes the panel;
//! - the session locked (ext-session-lock) with a lock screen on the
//!   output, all that shows, clicked and typed into; then unlocked, the
//!   window under it showing again.
//!
//! Linux only, and only under the headless sway, with `grim` and
//! `swaymsg` (`wtype` for typing, left out without it):
//!
//! ```sh
//! scripts/linux-run scripts/headless-wayland cargo test -p sidestep-ui --test wayland_desktop
//! ```
//!
//! Elsewhere it says so and passes.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::collections::HashMap;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use sidestep_ui::kurbo::{Point, Rect, Size};
    use sidestep_ui::layer_shell::{Anchors, KeyboardInteractivity, Layer, LayerOptions};
    use sidestep_ui::*;

    use crate::pointer;

    const DEADLINE: Duration = Duration::from_secs(60);
    const GREEN: Color = Color::rgb8(0x26, 0xa2, 0x69);
    const BAR: Color = Color::rgb8(0x1d, 0x2b, 0x53);
    const WALL: Color = Color::rgb8(0x7e, 0x25, 0x53);
    const MENU: Color = Color::rgb8(0xff, 0xa3, 0x00);
    /// The headless sway's output, in points.
    const OUTPUT: Size = Size::new(1280.0, 800.0);

    fn has(tool: &str) -> bool {
        Command::new("sh").arg("-c").arg(format!("command -v {tool}")).output().is_ok_and(|o| o.status.success())
    }

    /// A shell command's standard output, once it's done.
    fn sh(command: &str) -> String {
        let out = Command::new("sh").arg("-c").arg(command).stderr(Stdio::inherit()).output();
        out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    }

    fn swaymsg(args: &str) -> String {
        sh(&format!("swaymsg -s \"$(ls \"$XDG_RUNTIME_DIR\"/sway-ipc.*.sock | head -n1)\" {args}"))
    }

    /// The output as it shows now: its width and straight RGBA.
    struct Shot {
        width: u32,
        pixels: Vec<u8>,
    }

    impl Shot {
        fn take() -> Shot {
            Shot::of("")
        }

        /// Output `connector` alone (all of them for "").
        fn of(connector: &str) -> Shot {
            let path = format!("{}/shot.png", std::env::var("XDG_RUNTIME_DIR").expect("a runtime directory"));
            let _ = std::fs::remove_file(&path);
            let only = if connector.is_empty() { String::new() } else { format!("-o '{connector}' ") };
            sh(&format!("grim {only}'{path}'"));
            let image = Image::open(&path).expect("grim's screenshot");
            let (width, _) = image.pixel_size();
            Shot { width, pixels: image.to_rgba8().expect("its pixels") }
        }

        fn at(&self, x: u32, y: u32) -> Color {
            let i = (y * self.width + x) as usize * 4;
            let p = &self.pixels[i..i + 4];
            Color::rgba8(p[0], p[1], p[2], p[3])
        }

        /// Whether each point shows its color.
        fn shows(&self, points: &[((u32, u32), Color)]) -> bool {
            points.iter().all(|&((x, y), c)| near(self.at(x, y), c))
        }
    }

    fn near(a: Color, b: Color) -> bool {
        [(a.r, b.r), (a.g, b.g), (a.b, b.b)].iter().all(|(x, y)| (x - y).abs() < 3.0 / 255.0)
    }

    /// A decorated toplevel, which sway decorates itself: drawn, it shows
    /// its content from the output's top left (sway tiles it over the
    /// output, with no border), and sway knows it by its app id.
    struct Toplevel {
        started: Instant,
        drawn: bool,
        checked: bool,
    }

    impl Handler for Toplevel {
        fn launched(&mut self, cx: &mut Cx) {
            cx.open_window(WindowOptions::new("Desktop toplevel").size(400.0, 300.0));
            cx.set_repeating_timer(Duration::from_millis(20));
        }

        fn draw(&mut self, _cx: &mut Cx, _window: WindowId, canvas: &mut Canvas) {
            canvas.fill_rect(Rect::from_origin_size(Point::ZERO, canvas.size()), GREEN);
            self.drawn = true;
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            assert!(self.started.elapsed() < DEADLINE, "the toplevel never showed its content at the top left");
            // No title bar of its own above the content.
            if !self.drawn || !Shot::take().shows(&[((4, 4), GREEN), ((640, 400), GREEN)]) {
                return;
            }
            let tree = swaymsg("-t get_tree");
            assert!(tree.contains("\"app_id\": \"org.sidestep.DesktopTest\""), "sway's tree: {tree}");
            println!("  decorated by sway, known as org.sidestep.DesktopTest");
            // The output, as sway names and places it.
            let outputs = cx.outputs();
            println!("  outputs: {outputs:?}");
            let [output] = &outputs[..] else { panic!("one output: {outputs:?}") };
            assert_eq!(output.connector.as_deref(), Some("HEADLESS-1"));
            assert_eq!(output.frame, Rect::from_origin_size(Point::ZERO, OUTPUT));
            assert_eq!((output.scale, output.integer_scale), (1.0, 1));
            assert!(output.description.is_some() && !output.make.is_empty() && !output.model.is_empty());
            self.checked = true;
            cx.quit();
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Step {
        Showing,
        Pressed,
        Menu,
        Closed,
        Keyboard,
        Typed,
        Grown,
        Measured,
        Regions,
        Done,
    }

    /// A panel, a wallpaper and a window, then the panel's menu, keyboard
    /// and growth.
    struct Layers {
        started: Instant,
        step: Step,
        bar: Option<WindowId>,
        wallpaper: Option<WindowId>,
        window: Option<WindowId>,
        menu: Option<WindowId>,
        events: HashMap<WindowId, Vec<WindowEvent>>,
        /// Where each window's events stood when the step began.
        marks: HashMap<WindowId, usize>,
        pointer: mpsc::Sender<pointer::Do>,
        typing: bool,
        /// The step sent its input (it waits for what comes of it).
        asked: bool,
    }

    impl Layers {
        fn next(&mut self, step: Step) {
            self.step = step;
            self.asked = false;
            self.marks = self.events.iter().map(|(w, e)| (*w, e.len())).collect();
        }

        /// `window`'s events since the step began.
        fn since(&self, window: Option<WindowId>) -> &[WindowEvent] {
            let Some(w) = window else { return &[] };
            let events = self.events.get(&w).map_or(&[][..], Vec::as_slice);
            &events[self.marks.get(&w).copied().unwrap_or(0).min(events.len())..]
        }

        fn size(&self, cx: &mut Cx, window: Option<WindowId>) -> Option<Size> {
            cx.window(window?).map(|w| w.size())
        }

        fn point(&self, to: pointer::Do) {
            self.pointer.send(to).expect("the virtual pointer");
        }

        fn advance(&mut self, cx: &mut Cx) {
            let bar = self.bar.expect("the bar");
            match self.step {
                // The panel as wide as the output and as tall as it asked,
                // over the window, which sway tiles under it; the wallpaper
                // the output's size.
                Step::Showing => {
                    let below = Size::new(OUTPUT.width, OUTPUT.height - 24.0);
                    if self.size(cx, self.bar) != Some(Size::new(OUTPUT.width, 24.0))
                        || self.size(cx, self.wallpaper) != Some(OUTPUT)
                        || self.size(cx, self.window) != Some(below)
                        || !Shot::take().shows(&[((5, 5), BAR), ((1275, 20), BAR), ((640, 30), GREEN)])
                    {
                        return;
                    }
                    println!("  a panel along the top, a window tiled under it");
                    self.point(pointer::Do::MoveTo(40, 12));
                    self.point(pointer::Do::Click);
                    self.next(Step::Pressed);
                }
                // The press reaches the panel where it was.
                Step::Pressed => {
                    let Some(at) = self.since(self.bar).iter().find_map(|e| match e {
                        WindowEvent::PointerButton { position, pressed: true, .. } => Some(*position),
                        _ => None,
                    }) else {
                        return;
                    };
                    assert!(at.distance(Point::new(40.0, 12.0)) < 1.0, "{at:?}");
                    self.next(Step::Menu);
                }
                // The menu below the press, over the window.
                Step::Menu => {
                    if self.size(cx, self.menu) != Some(Size::new(160.0, 120.0))
                        || !Shot::take().shows(&[((40, 30), MENU), ((180, 140), MENU), ((200, 30), GREEN)])
                    {
                        return;
                    }
                    println!("  a menu from the panel");
                    // (Every surface on the output is the program's, so a click
                    // elsewhere would go to one of them rather than dismiss it.)
                    cx.close_window(self.menu.expect("the menu"));
                    self.next(Step::Closed);
                }
                Step::Closed => {
                    if !Shot::take().shows(&[((40, 30), GREEN)]) {
                        return;
                    }
                    println!("  closed");
                    cx.window(bar).expect("the bar").set_layer_keyboard(KeyboardInteractivity::Exclusive);
                    self.next(Step::Keyboard);
                }
                // Asked to, the panel takes the keyboard from the window.
                // The headless sway has no keyboard of its own, so the window
                // heard of focus from its activation, which it loses.
                Step::Keyboard => {
                    if !self.since(self.window).contains(&WindowEvent::Focused(false)) {
                        return;
                    }
                    if self.typing {
                        // wtype's keyboard enters the panel.
                        let _ = std::thread::spawn(|| sh("wtype -k Shift_L 'k'"));
                        self.next(Step::Typed);
                    } else {
                        println!("  the window lost the keyboard (no wtype: no typing)");
                        self.grow(cx);
                    }
                }
                Step::Typed => {
                    let typed: String = self
                        .since(self.bar)
                        .iter()
                        .filter_map(|e| if let WindowEvent::Key(k) = e { k.text.clone() } else { None })
                        .collect();
                    if typed != "k" {
                        return;
                    }
                    assert!(self.since(self.bar).contains(&WindowEvent::Focused(true)));
                    println!("  the panel has the keyboard, and was typed into");
                    self.grow(cx);
                }
                // Taller, with a zone as tall: the window makes room.
                Step::Grown => {
                    let below = Size::new(OUTPUT.width, OUTPUT.height - 32.0);
                    if self.size(cx, self.bar) != Some(Size::new(OUTPUT.width, 32.0))
                        || self.size(cx, self.window) != Some(below)
                        || !Shot::take().shows(&[((5, 30), BAR), ((640, 34), GREEN)])
                    {
                        return;
                    }
                    println!("  grown, and the window made room");
                    cx.set_frame_stats(Some(Duration::from_millis(100)));
                    self.next(Step::Measured);
                }
                // The window's frames measured as it's redrawn.
                Step::Measured => {
                    let window = self.window.expect("the window");
                    cx.window(window).expect("the window").redraw();
                    let stats = cx.frame_stats();
                    let Some(s) = stats.iter().find(|s| s.window == window && s.frames >= 2) else { return };
                    println!("  measured: {s:?}");
                    assert!(s.rasterize.max > Duration::ZERO && s.present.max > Duration::ZERO, "{s:?}");
                    assert!(s.damage > 0.99, "{s:?}");
                    cx.set_frame_stats(None);
                    // The panel takes clicks on its left half only: on the
                    // right, they go to the wallpaper under it.
                    let left = Rect::new(0.0, 0.0, OUTPUT.width / 2.0, 32.0);
                    cx.window(bar).expect("the bar").set_input_region(Some(&[left]));
                    self.next(Step::Regions);
                }
                Step::Regions => {
                    let pressed = |events: &[WindowEvent]| {
                        events.iter().find_map(|e| match e {
                            WindowEvent::PointerButton { position, pressed: true, .. } => Some(*position),
                            _ => None,
                        })
                    };
                    if !std::mem::replace(&mut self.asked, true) {
                        self.point(pointer::Do::MoveTo(900, 16));
                        self.point(pointer::Do::Click);
                        self.point(pointer::Do::MoveTo(100, 16));
                        self.point(pointer::Do::Click);
                    }
                    let (Some(through), Some(taken)) =
                        (pressed(self.since(self.wallpaper)), pressed(self.since(self.bar)))
                    else {
                        return;
                    };
                    assert!(through.distance(Point::new(900.0, 16.0)) < 1.0, "{through:?}");
                    assert!(taken.distance(Point::new(100.0, 16.0)) < 1.0, "{taken:?}");
                    let presses = |w| {
                        self.since(w)
                            .iter()
                            .filter(|e| matches!(e, WindowEvent::PointerButton { pressed: true, .. }))
                            .count()
                    };
                    assert_eq!((presses(self.bar), presses(self.wallpaper)), (1, 1));
                    println!("  the panel took clicks in its input region, the wallpaper the others");
                    self.point(pointer::Do::Done);
                    self.next(Step::Done);
                    cx.quit();
                }
                Step::Done => {}
            }
        }

        fn grow(&mut self, cx: &mut Cx) {
            let mut bar = cx.window(self.bar.expect("the bar")).expect("the bar");
            bar.set_layer_keyboard(KeyboardInteractivity::None);
            bar.set_layer_size(Size::new(0.0, 32.0));
            bar.set_layer_exclusive_zone(32);
            self.next(Step::Grown);
        }
    }

    impl Handler for Layers {
        fn launched(&mut self, cx: &mut Cx) {
            let wallpaper = LayerOptions::new(Layer::Background, "wallpaper").anchors(Anchors::ALL).exclusive_zone(-1);
            self.wallpaper = Some(cx.open_window(WindowOptions::layer(wallpaper)));
            let panel = LayerOptions::new(Layer::Top, "panel")
                .anchors(Anchors::TOP | Anchors::LEFT | Anchors::RIGHT)
                .exclusive_zone(24);
            self.bar = Some(cx.open_window(WindowOptions::layer(panel).size(0.0, 24.0)));
            self.window = Some(cx.open_window(WindowOptions::new("Desktop window")));
            cx.set_repeating_timer(Duration::from_millis(20));
        }

        fn window_event(&mut self, cx: &mut Cx, window: WindowId, event: WindowEvent) {
            // The press on the panel opens its menu, at once (a grab
            // needs the press's serial).
            if Some(window) == self.bar
                && self.step == Step::Pressed
                && self.menu.is_none()
                && matches!(event, WindowEvent::PointerButton { pressed: true, .. })
            {
                let anchor = Rect::new(30.0, 0.0, 90.0, 24.0);
                self.menu = Some(cx.open_window(WindowOptions::popup(window, anchor, Size::new(160.0, 120.0), true)));
            }
            self.events.entry(window).or_default().push(event);
        }

        fn draw(&mut self, _cx: &mut Cx, window: WindowId, canvas: &mut Canvas) {
            let color = match Some(window) {
                w if w == self.bar => BAR,
                w if w == self.wallpaper => WALL,
                w if w == self.menu => MENU,
                _ => GREEN,
            };
            canvas.fill_rect(Rect::from_origin_size(Point::ZERO, canvas.size()), color);
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            assert!(self.started.elapsed() < DEADLINE, "stuck at {:?}: {:?}", self.step, self.events);
            self.advance(cx);
        }
    }

    pub fn run() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() || std::env::var("WLR_BACKENDS").as_deref() != Ok("headless") {
            println!("skipped: needs the headless sway (run it under scripts/headless-wayland)");
            return;
        }
        if !has("grim") || !has("swaymsg") {
            println!("skipped: needs grim and swaymsg");
            return;
        }
        // SAFETY: nothing else runs yet.
        unsafe { std::env::set_var("SIDESTEP_APPEARANCE", "light") };
        let toplevel = Toplevel { started: Instant::now(), drawn: false, checked: false };
        let toplevel = App::new().app_id("org.sidestep.DesktopTest").run(toplevel).expect("a compositor");
        assert!(toplevel.checked);

        let (tx, rx) = mpsc::channel();
        let pointing = std::thread::spawn(move || pointer::run(rx));
        let layers = Layers {
            started: Instant::now(),
            step: Step::Showing,
            bar: None,
            wallpaper: None,
            window: None,
            menu: None,
            events: HashMap::new(),
            marks: HashMap::new(),
            pointer: tx,
            typing: has("wtype"),
            asked: false,
        };
        let layers = App::new().run(layers).expect("a compositor");
        assert_eq!(layers.step, Step::Done, "every step ran");
        drop(layers);
        pointing.join().expect("the virtual pointer");

        let outputs = Outputs { started: Instant::now(), step: Plug::Waiting, changed: 0, panel: None, closing: false };
        let outputs = App::new().quit_when_last_window_closes(false).run(outputs).expect("a compositor");
        assert_eq!(outputs.step, Plug::Done);

        let (tx, rx) = mpsc::channel();
        let pointing = std::thread::spawn(move || pointer::run(rx));
        let locking = Locking {
            started: Instant::now(),
            step: Lock::Showing,
            window: None,
            lock: None,
            locked: false,
            events: Vec::new(),
            pointer: tx,
            typing: has("wtype"),
        };
        let locking = App::new().run(locking).expect("a compositor");
        assert_eq!(locking.step, Lock::Done);
        drop(locking);
        pointing.join().expect("the virtual pointer");
        println!("wayland_desktop: ok");
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Plug {
        Waiting,
        Plugged,
        Panel,
        Unplugged,
        Done,
    }

    /// An output plugged in (sway's `create_output`, put to the right of
    /// the first), a panel put on it, and the output unplugged, which
    /// takes the panel away.
    struct Outputs {
        started: Instant,
        step: Plug,
        changed: usize,
        panel: Option<WindowId>,
        closing: bool,
    }

    impl Outputs {
        fn second(cx: &Cx) -> Option<Output> {
            cx.outputs().into_iter().find(|o| o.connector.as_deref() == Some("HEADLESS-2"))
        }
    }

    impl Handler for Outputs {
        fn launched(&mut self, cx: &mut Cx) {
            cx.set_repeating_timer(Duration::from_millis(20));
        }

        fn outputs_changed(&mut self, _cx: &mut Cx) {
            self.changed += 1;
        }

        fn window_event(&mut self, _cx: &mut Cx, _window: WindowId, event: WindowEvent) {
            self.closing |= event == WindowEvent::CloseRequested;
        }

        fn draw(&mut self, _cx: &mut Cx, _window: WindowId, canvas: &mut Canvas) {
            canvas.fill_rect(Rect::from_origin_size(Point::ZERO, canvas.size()), BAR);
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            assert!(self.started.elapsed() < DEADLINE, "stuck at {:?}: {:?}", self.step, cx.outputs());
            match self.step {
                Plug::Waiting => {
                    if cx.outputs().len() != 1 {
                        return;
                    }
                    swaymsg("create_output >/dev/null");
                    swaymsg("output HEADLESS-2 mode 800x600 position 1280 0 >/dev/null");
                    self.changed = 0;
                    self.step = Plug::Plugged;
                }
                // Heard of, where it was put.
                Plug::Plugged => {
                    let Some(second) = Outputs::second(cx) else { return };
                    if second.frame != Rect::new(1280.0, 0.0, 2080.0, 600.0) || self.changed == 0 {
                        return;
                    }
                    println!("  plugged in: {second:?}");
                    let panel = LayerOptions::new(Layer::Top, "panel")
                        .anchors(Anchors::TOP | Anchors::LEFT | Anchors::RIGHT)
                        .exclusive_zone(24)
                        .output(second.id);
                    self.panel = Some(cx.open_window(WindowOptions::layer(panel).size(0.0, 24.0)));
                    self.step = Plug::Panel;
                }
                // The panel on it, as wide as it is.
                Plug::Panel => {
                    let panel = self.panel.expect("the panel");
                    let second = Outputs::second(cx).expect("the second output");
                    let Some(w) = cx.window(panel) else { return };
                    if w.size() != Size::new(800.0, 24.0) || w.outputs() != [second.id] {
                        return;
                    }
                    if !Shot::of("HEADLESS-2").shows(&[((5, 5), BAR), ((795, 20), BAR)]) {
                        return;
                    }
                    println!("  a panel on it");
                    swaymsg("output HEADLESS-2 unplug >/dev/null");
                    self.step = Plug::Unplugged;
                }
                // Gone, and the panel with it: the compositor closed it.
                Plug::Unplugged => {
                    if Outputs::second(cx).is_some() || self.panel.is_some_and(|p| cx.window(p).is_some()) {
                        return;
                    }
                    assert!(self.closing, "the panel was asked to close");
                    println!("  unplugged, and the panel closed");
                    self.step = Plug::Done;
                    cx.quit();
                }
                Plug::Done => {}
            }
        }
    }

    const LOCK: Color = Color::rgb8(0x00, 0x87, 0x51);

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Lock {
        Showing,
        Locked,
        Clicked,
        Typed,
        Unlocked,
        Done,
    }

    /// A window, then the session locked with a lock screen on the output,
    /// which shows instead of the window and takes clicks and keys; then
    /// unlocked, the lock screen gone and the window showing again.
    struct Locking {
        started: Instant,
        step: Lock,
        window: Option<WindowId>,
        lock: Option<WindowId>,
        locked: bool,
        /// The lock screen's.
        events: Vec<WindowEvent>,
        pointer: mpsc::Sender<pointer::Do>,
        typing: bool,
    }

    impl Handler for Locking {
        fn launched(&mut self, cx: &mut Cx) {
            self.window = Some(cx.open_window(WindowOptions::new("Desktop window")));
            cx.set_repeating_timer(Duration::from_millis(20));
        }

        fn session_locked(&mut self, _cx: &mut Cx) {
            self.locked = true;
        }

        fn session_lock_finished(&mut self, _cx: &mut Cx) {
            panic!("sway refused or ended the lock");
        }

        fn window_event(&mut self, _cx: &mut Cx, window: WindowId, event: WindowEvent) {
            if Some(window) == self.lock {
                self.events.push(event);
            }
        }

        fn draw(&mut self, _cx: &mut Cx, window: WindowId, canvas: &mut Canvas) {
            let color = if Some(window) == self.lock { LOCK } else { GREEN };
            canvas.fill_rect(Rect::from_origin_size(Point::ZERO, canvas.size()), color);
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            assert!(self.started.elapsed() < DEADLINE, "stuck at {:?}: {:?}", self.step, self.events);
            let size = |cx: &mut Cx, w: Option<WindowId>| w.and_then(|w| cx.window(w).map(|w| w.size()));
            match self.step {
                Lock::Showing => {
                    let outputs = cx.outputs();
                    if outputs.is_empty() || !Shot::take().shows(&[((640, 400), GREEN)]) {
                        return;
                    }
                    // A lock screen on every output, at once.
                    cx.lock_session();
                    for output in outputs {
                        self.lock = Some(cx.open_window(WindowOptions::lock(output.id)));
                    }
                    self.step = Lock::Locked;
                }
                // Locked, the lock screen the output's size and all that
                // shows.
                Lock::Locked => {
                    if !self.locked
                        || size(cx, self.lock) != Some(OUTPUT)
                        || !Shot::take().shows(&[((5, 5), LOCK), ((640, 400), LOCK)])
                    {
                        return;
                    }
                    assert!(cx.is_session_locked());
                    println!("  locked, the lock screen showing");
                    self.pointer.send(pointer::Do::MoveTo(300, 200)).expect("the virtual pointer");
                    self.pointer.send(pointer::Do::Click).expect("the virtual pointer");
                    self.step = Lock::Clicked;
                }
                Lock::Clicked => {
                    let Some(at) = self.events.iter().find_map(|e| match e {
                        WindowEvent::PointerButton { position, pressed: true, .. } => Some(*position),
                        _ => None,
                    }) else {
                        return;
                    };
                    assert!(at.distance(Point::new(300.0, 200.0)) < 1.0, "{at:?}");
                    if self.typing {
                        // wtype's first press goes while the compositor takes its
                        // keymap.
                        let _ = std::thread::spawn(|| sh("wtype -k Shift_L 'pw'"));
                        self.step = Lock::Typed;
                    } else {
                        println!("  clicked (no wtype: no typing)");
                        self.unlock(cx);
                    }
                }
                Lock::Typed => {
                    let typed: String = self
                        .events
                        .iter()
                        .filter_map(|e| if let WindowEvent::Key(k) = e { k.text.clone() } else { None })
                        .collect();
                    if typed != "pw" {
                        return;
                    }
                    assert!(self.events.contains(&WindowEvent::Focused(true)));
                    println!("  clicked and typed into");
                    self.unlock(cx);
                }
                // Unlocked: the lock screen gone, the window showing.
                Lock::Unlocked => {
                    if !Shot::take().shows(&[((640, 400), GREEN)]) {
                        return;
                    }
                    println!("  unlocked, the window showing again");
                    self.pointer.send(pointer::Do::Done).expect("the virtual pointer");
                    self.step = Lock::Done;
                    cx.quit();
                }
                Lock::Done => {}
            }
        }
    }

    impl Locking {
        fn unlock(&mut self, cx: &mut Cx) {
            cx.unlock_session();
            assert!(self.lock.is_some_and(|l| cx.window(l).is_none()), "the lock screen closed");
            assert!(!cx.is_session_locked());
            self.step = Lock::Unlocked;
        }
    }
}

/// A virtual pointer (see the file); this test turns no wheel.
#[cfg(not(target_vendor = "apple"))]
#[allow(dead_code)]
#[path = "support/pointer.rs"]
mod pointer;
