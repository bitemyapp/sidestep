//! A desktop environment's surfaces under a real compositor, the headless
//! sway: a toplevel names its program (its app id, as sway lists it) and
//! is decorated by the compositor, which sway prefers, drawing nothing of
//! its own (a screenshot with `grim` shows its content from its top
//! left). Linux only, and only under the headless sway, with `grim` and
//! `swaymsg`:
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
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use sidestep_ui::kurbo::{Point, Rect};
    use sidestep_ui::*;

    const DEADLINE: Duration = Duration::from_secs(60);
    const GREEN: Color = Color::rgb8(0x26, 0xa2, 0x69);

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

    /// The output as it shows now: width, height and straight RGBA.
    struct Shot {
        width: u32,
        pixels: Vec<u8>,
    }

    impl Shot {
        fn take() -> Shot {
            let path = format!("{}/shot.png", std::env::var("XDG_RUNTIME_DIR").expect("a runtime directory"));
            let _ = std::fs::remove_file(&path);
            sh(&format!("grim '{path}'"));
            let image = Image::open(&path).expect("grim's screenshot");
            let (width, _) = image.pixel_size();
            Shot { width, pixels: image.to_rgba8().expect("its pixels") }
        }

        fn at(&self, x: u32, y: u32) -> Color {
            let i = (y * self.width + x) as usize * 4;
            let p = &self.pixels[i..i + 4];
            Color::rgba8(p[0], p[1], p[2], p[3])
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
        window: Option<WindowId>,
        drawn: bool,
        checked: bool,
    }

    impl Handler for Toplevel {
        fn launched(&mut self, cx: &mut Cx) {
            self.window = Some(cx.open_window(WindowOptions::new("Desktop toplevel").size(400.0, 300.0)));
            cx.set_repeating_timer(Duration::from_millis(20));
        }

        fn draw(&mut self, _cx: &mut Cx, _window: WindowId, canvas: &mut Canvas) {
            canvas.fill_rect(Rect::from_origin_size(Point::ZERO, canvas.size()), GREEN);
            self.drawn = true;
        }

        fn timer(&mut self, cx: &mut Cx, _timer: TimerId) {
            assert!(self.started.elapsed() < DEADLINE, "the toplevel never showed its content at the top left");
            if !self.drawn {
                return;
            }
            let shot = Shot::take();
            // No title bar of its own above the content.
            if !near(shot.at(4, 4), GREEN) || !near(shot.at(640, 400), GREEN) {
                return;
            }
            let tree = swaymsg("-t get_tree");
            assert!(tree.contains("\"app_id\": \"org.sidestep.DesktopTest\""), "sway's tree: {tree}");
            println!("  decorated by sway, known as org.sidestep.DesktopTest");
            self.checked = true;
            cx.quit();
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
        let toplevel = Toplevel { started: Instant::now(), window: None, drawn: false, checked: false };
        let toplevel = App::new().app_id("org.sidestep.DesktopTest").run(toplevel).expect("a compositor");
        assert!(toplevel.checked);
        println!("wayland_desktop: ok");
    }
}
