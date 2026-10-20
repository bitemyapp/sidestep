//! A book end to end: a drawn story and an app story rendered in child
//! processes of this test (each child runs this `main` again, which hands
//! it to the book), their golden images written, checked, one changed and
//! checked again, which fails and leaves the picture made and a diff.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use sidestep_stories::{Book, Knobs, Picture};
    use sidestep_ui::kurbo::{Point, Rect, Size};
    use sidestep_ui::*;

    /// A program whose window is the accent, with a white square where it
    /// was last clicked.
    #[derive(Default)]
    struct Clicks {
        at: Option<Point>,
        window: Option<WindowId>,
    }

    impl Handler for Clicks {
        fn launched(&mut self, cx: &mut Cx) {
            self.window = Some(cx.open_window(WindowOptions::new("Clicks").size(40.0, 30.0)));
        }

        fn window_event(&mut self, cx: &mut Cx, window: WindowId, event: WindowEvent) {
            if let WindowEvent::PointerButton { position, pressed: true, .. } = event {
                self.at = Some(position);
                if let Some(mut w) = cx.window(window) {
                    w.invalidate(Rect::new(0.0, 0.0, 40.0, 30.0));
                }
            }
        }

        fn draw(&mut self, _cx: &mut Cx, _window: WindowId, canvas: &mut Canvas) {
            let accent = canvas.system_color(SystemColor::Accent);
            canvas.fill_rect(Rect::new(0.0, 0.0, 40.0, 30.0), accent);
            if let Some(at) = self.at {
                canvas.fill_rect(Rect::from_center_size(at, (4.0, 4.0)), Color::WHITE);
            }
        }
    }

    fn book(golden: &Path) -> Book {
        let mut book = Book::new(golden);
        book.drawn("drawn/half", Size::new(20.0, 10.0), |canvas, _| {
            let accent = canvas.system_color(SystemColor::Accent);
            canvas.fill_rect(Rect::new(0.0, 0.0, 10.0, 10.0), accent);
        })
        .variants([Knobs::LIGHT, Knobs::LIGHT.accent(0xff0000).at(2.0)]);
        book.app("app/clicked", |_| Clicks::default())
            .step(|_, d| d.click(Point::new(10.0, 10.0)))
            .variants([Knobs::LIGHT.accent(0x00ff00), Knobs::DARK.accent(0x0000ff).at(2.0)]);
        book
    }

    fn at(p: &Picture, x: u32, y: u32) -> [u8; 4] {
        let i = (y * p.width + x) as usize * 4;
        p.rgba[i..i + 4].try_into().unwrap()
    }

    fn check(golden: &Path, args: &[&str]) -> ExitCode {
        book(golden).run_with(args.iter().map(|s| (*s).to_owned()).collect())
    }

    pub fn run() {
        let dir = std::env::temp_dir().join(format!("sidestep-stories-book-{}", std::process::id()));
        // A child render: the book takes it from here.
        if std::env::var_os("SIDESTEP_STORIES_CHILD").is_some() {
            std::process::exit(if book(&dir).run_with(Vec::new()) == ExitCode::SUCCESS { 0 } else { 1 });
        }
        let golden: PathBuf = dir.join("golden");
        // SAFETY: nothing else runs yet.
        unsafe { std::env::set_var("STORIES_OUT", dir.join("out")) };

        let b = book(&golden);
        let names: Vec<&str> = b.stories().iter().map(|s| s.name()).collect();
        assert_eq!(names, ["drawn/half", "app/clicked"]);

        // A drawing: the accent on the left half, transparent on the right.
        let drawn = b.story("drawn/half").unwrap();
        let red = b.render(drawn, &Knobs::LIGHT.accent(0xff0000).at(2.0)).expect("drawn");
        assert_eq!((red.width, red.height), (40, 20));
        assert_eq!(at(&red, 5, 5), [255, 0, 0, 255]);
        assert_eq!(at(&red, 30, 5)[3], 0, "transparent");

        // A program, clicked: its window at the scale, in its accent, with
        // the click's square.
        let app = b.story("app/clicked").unwrap();
        let blue = b.render(app, &Knobs::DARK.accent(0x0000ff).at(2.0)).expect("app");
        assert_eq!((blue.width, blue.height), (80, 60));
        assert_eq!(at(&blue, 60, 40), [0, 0, 255, 255], "the accent");
        assert_eq!(at(&blue, 20, 20), [255, 255, 255, 255], "where it was clicked");
        let fractional = b.render(app, &Knobs::LIGHT.at(1.5));
        assert!(fractional.unwrap_err().contains("whole scale"));

        // Golden images written, then matched.
        assert_eq!(check(&golden, &["check", "--update"]), ExitCode::SUCCESS);
        assert!(golden.join("app/clicked/dark+0000ff@2x.png").is_file());
        assert_eq!(check(&golden, &["check"]), ExitCode::SUCCESS);
        assert_eq!(check(&golden, &["check", "drawn"]), ExitCode::SUCCESS, "a filter");
        assert_ne!(check(&golden, &["check", "nothing-matches"]), ExitCode::SUCCESS);

        // A golden image changed: the check fails and leaves what it made
        // and a diff.
        let path = golden.join("app/clicked/light+00ff00@1x.png");
        let mut changed = Picture::open(&path).unwrap();
        changed.rgba[0] = changed.rgba[0].wrapping_add(100);
        changed.save(&path).unwrap();
        assert_eq!(check(&golden, &["check"]), ExitCode::FAILURE);
        let failures = dir.join("out/failures/app/clicked");
        let diff = Picture::open(&failures.join("light+00ff00@1x.diff.png")).unwrap();
        assert_eq!(at(&diff, 0, 0), [255, 0, 0, 255], "the pixel that differs");
        assert!(failures.join("light+00ff00@1x.png").is_file());

        let _ = std::fs::remove_dir_all(&dir);
        println!("book: ok");
    }
}
