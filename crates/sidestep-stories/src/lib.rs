//! Stories for sidestep-ui programs, as Storybook has for web components:
//! each story is one named state of a drawing or of a whole program, shown
//! in chosen [`Knobs`] (light or dark, high contrast, scale, accent),
//! rendered without a display, and checked against a golden image.
//!
//! A project gathers its stories in a [`Book`] and gives the book a
//! binary of its own:
//!
// On macOS the crate is empty, so the example isn't built there.
#![cfg_attr(not(target_vendor = "apple"), doc = "```no_run")]
#![cfg_attr(target_vendor = "apple", doc = "```ignore")]
//! use sidestep_stories::{Book, Knobs};
//! use sidestep_ui::{Color, SystemColor};
//! use sidestep_ui::kurbo::{Rect, Size};
//!
//! fn main() -> std::process::ExitCode {
//!     let mut book = Book::new(concat!(env!("CARGO_MANIFEST_DIR"), "/golden"));
//!     book.drawn("swatch/accent", Size::new(64.0, 64.0), |canvas, _knobs| {
//!         let accent = canvas.system_color(SystemColor::Accent);
//!         canvas.fill_rect(Rect::new(8.0, 8.0, 56.0, 56.0), accent);
//!     })
//!     .variants([Knobs::LIGHT, Knobs::DARK.at(2.0)]);
//!     book.run()
//! }
//! ```
//!
//! The binary takes a command:
//!
//! - `list`: every story, its kind and its variants.
//! - `render NAME [KNOBS] [--out FILE]`: one story in one variant (its
//!   first unless `KNOBS`, such as `dark@2x`, says otherwise) into a PNG.
//! - `check [FILTER] [--update]`: every story (whose name contains
//!   `FILTER`) in every variant, compared with its golden image,
//!   `GOLDEN_DIR/NAME/KNOBS.png`. `--update` (or `STORIES_UPDATE=1`)
//!   writes the golden images instead. What differs is written to
//!   `target/stories/failures/` (`STORIES_OUT` moves `target/stories`):
//!   the picture made and, where the sizes match, a diff.
//!
//! **Kinds.** A *drawn* story ([`Book::drawn`]) draws on a canvas, through
//! [`sidestep_ui::offscreen`]. An *app* story ([`Book::app`]) runs a whole
//! program, its [`Handler`](sidestep_ui::Handler), under the null render
//! thread ([`sidestep_ui::testing`]), plays its steps (clicks, typing,
//! keys, through a [`Driver`]) and takes its first window's pixels.
//!
//! **Isolation.** Every render runs in a child process of the book's
//! binary: one program per process, as the toolkit wants, with the knobs
//! in its environment, and away from the desktop the binary runs in. The
//! child has no Wayland or X display and no session bus, so neither the
//! desktop's settings nor its compositor reach a story: what a story looks
//! like depends only on its knobs and the system's fonts
//! (`SIDESTEP_FONT` and `SIDESTEP_MONO_FONT` name font files instead).
#![cfg(not(target_vendor = "apple"))]

mod driver;
mod knobs;
mod picture;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sidestep_ui::kurbo::Size;
use sidestep_ui::{Canvas, Handler};

pub use driver::Driver;
pub use knobs::Knobs;
pub use picture::{Comparison, Picture};

/// The variable a child render is told what to render by.
const CHILD: &str = "SIDESTEP_STORIES_CHILD";

/// A story's kind, as `list` shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Drawn,
    App,
}

type Render = Box<dyn Fn(&Knobs) -> Result<Picture, String>>;

/// One story: its name, how to render it, and what it is checked in.
pub struct Story {
    name: String,
    kind: Kind,
    variants: Vec<Knobs>,
    threshold: u8,
    tolerance: f64,
    render: Render,
}

impl Story {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The knobs it is checked in, the first its default.
    pub fn variants(&self) -> &[Knobs] {
        &self.variants
    }

    /// Render it in `knobs`, in this process. A program can run only once
    /// in a process; [`Book::render`] runs it in a child instead.
    pub fn render_here(&self, knobs: &Knobs) -> Result<Picture, String> {
        (self.render)(knobs)
    }
}

/// Settings for a story just added to a [`Book`].
pub struct StoryOptions<'a> {
    story: &'a mut Story,
}

impl StoryOptions<'_> {
    /// The knobs the story is checked in (light at 1× unless set).
    pub fn variants(self, variants: impl IntoIterator<Item = Knobs>) -> Self {
        let variants: Vec<Knobs> = variants.into_iter().collect();
        assert!(!variants.is_empty(), "story {}: no variants", self.story.name);
        self.story.variants = variants;
        self
    }

    /// How far a channel may differ before a pixel counts as different (2
    /// unless set): rounding in blending and text rasterization.
    pub fn threshold(self, threshold: u8) -> Self {
        self.story.threshold = threshold;
        self
    }

    /// The fraction of pixels that may differ (none unless set).
    pub fn tolerance(self, fraction: f64) -> Self {
        self.story.tolerance = fraction.clamp(0.0, 1.0);
        self
    }
}

/// Settings and steps for an app story just added to a [`Book`].
pub struct AppStoryOptions<'a, H> {
    options: StoryOptions<'a>,
    steps: std::rc::Rc<std::cell::RefCell<Vec<driver::Step<H>>>>,
}

impl<H> AppStoryOptions<'_, H> {
    /// Add a step: what a person does next, given the program (to ask it
    /// where things are) and a [`Driver`]. Each step plays once the
    /// program has drawn what the last one changed.
    pub fn step(self, step: impl Fn(&mut H, &mut Driver<'_>) + 'static) -> Self {
        self.steps.borrow_mut().push(Box::new(step));
        self
    }

    /// See [`StoryOptions::variants`]; a program's window takes whole
    /// scales only.
    pub fn variants(self, variants: impl IntoIterator<Item = Knobs>) -> Self {
        AppStoryOptions { options: self.options.variants(variants), steps: self.steps }
    }

    /// See [`StoryOptions::threshold`].
    pub fn threshold(self, threshold: u8) -> Self {
        AppStoryOptions { options: self.options.threshold(threshold), steps: self.steps }
    }

    /// See [`StoryOptions::tolerance`].
    pub fn tolerance(self, fraction: f64) -> Self {
        AppStoryOptions { options: self.options.tolerance(fraction), steps: self.steps }
    }
}

/// A project's stories, and the command line that renders and checks
/// them.
pub struct Book {
    stories: Vec<Story>,
    golden: PathBuf,
}

impl Book {
    /// A book whose golden images live in `golden`.
    pub fn new(golden: impl Into<PathBuf>) -> Book {
        Book { stories: Vec::new(), golden: golden.into() }
    }

    pub fn stories(&self) -> &[Story] {
        &self.stories
    }

    pub fn story(&self, name: &str) -> Option<&Story> {
        self.stories.iter().find(|s| s.name == name)
    }

    fn add(&mut self, name: &str, kind: Kind, render: Render) -> StoryOptions<'_> {
        let valid = !name.is_empty()
            && !name.starts_with('/')
            && !name.ends_with('/')
            && !name.contains("//")
            && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_/".contains(c));
        assert!(valid, "story name {name:?}: use a-z, 0-9, '-', '_' and '/' between parts");
        assert!(self.story(name).is_none(), "story {name:?} is in the book twice");
        self.stories.push(Story {
            name: name.to_owned(),
            kind,
            variants: vec![Knobs::LIGHT],
            threshold: 2,
            tolerance: 0.0,
            render,
        });
        StoryOptions { story: self.stories.last_mut().expect("just added") }
    }

    /// A story drawn on a canvas `size` points big by `draw`.
    pub fn drawn(&mut self, name: &str, size: Size, draw: impl Fn(&mut Canvas, &Knobs) + 'static) -> StoryOptions<'_> {
        let render = move |knobs: &Knobs| {
            sidestep_engine::palette::set_accent(knobs.accent.map(|rgb| {
                let channel = |shift: u32| ((rgb >> shift) & 0xff) as f32 / 255.0;
                [channel(16), channel(8), channel(0), 1.0]
            }));
            let image = sidestep_ui::offscreen::render(size, knobs.scale, knobs.appearance, |c| draw(c, knobs));
            let (width, height) = image.pixel_size();
            let rgba = image.to_rgba8().ok_or("the drawing kept no pixels")?;
            Ok(Picture { width, height, rgba })
        };
        self.add(name, Kind::Drawn, Box::new(render))
    }

    /// A story of a whole program, made by `make` for the knobs it runs
    /// in; its steps follow ([`AppStoryOptions::step`]).
    pub fn app<H: Handler + 'static>(
        &mut self,
        name: &str,
        make: impl Fn(&Knobs) -> H + 'static,
    ) -> AppStoryOptions<'_, H> {
        let steps: std::rc::Rc<std::cell::RefCell<Vec<driver::Step<H>>>> = Default::default();
        let played = steps.clone();
        let render = move |knobs: &Knobs| driver::run(make(knobs), &played.borrow(), knobs);
        AppStoryOptions { options: self.add(name, Kind::App, Box::new(render)), steps }
    }

    /// Render `story` in `knobs` in a child process of this binary.
    pub fn render(&self, story: &Story, knobs: &Knobs) -> Result<Picture, String> {
        render_in_child(&story.name, knobs)
    }

    /// Run the command line (see the crate's documentation), or a child's
    /// render.
    pub fn run(self) -> ExitCode {
        self.run_with(std::env::args().skip(1).collect())
    }

    /// [`run`](Book::run) with these arguments rather than the process's.
    pub fn run_with(self, args: Vec<String>) -> ExitCode {
        if let Ok(child) = std::env::var(CHILD) {
            return self.run_child(&child);
        }
        let result = match args.first().map(String::as_str) {
            Some("list") => {
                self.list();
                Ok(true)
            }
            Some("render") => self.render_command(&args[1..]),
            Some("check") => self.check(&args[1..]),
            _ => Err(USAGE.to_owned()),
        };
        match result {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(2)
            }
        }
    }

    fn run_child(&self, child: &str) -> ExitCode {
        let mut parts = child.splitn(3, '\n');
        let (Some(name), Some(id), Some(out)) = (parts.next(), parts.next(), parts.next()) else {
            eprintln!("{CHILD} is not NAME, KNOBS and OUT on lines of their own");
            return ExitCode::from(2);
        };
        let result = self
            .story(name)
            .ok_or_else(|| format!("no story {name:?}"))
            .and_then(|story| {
                let knobs = Knobs::parse(id).ok_or_else(|| format!("{id:?} are not knobs"))?;
                story.render_here(&knobs)
            })
            .and_then(|picture| picture.save(Path::new(out)));
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{name} {id}: {e}");
                ExitCode::FAILURE
            }
        }
    }

    fn list(&self) {
        for story in &self.stories {
            let kind = match story.kind {
                Kind::Drawn => "drawn",
                Kind::App => "app",
            };
            let variants: Vec<String> = story.variants.iter().map(Knobs::id).collect();
            println!("{}\t{kind}\t{}", story.name, variants.join(" "));
        }
    }

    fn render_command(&self, args: &[String]) -> Result<bool, String> {
        let mut name = None;
        let mut knobs = None;
        let mut out = None;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--out" => out = Some(PathBuf::from(args.next().ok_or("--out needs a file")?)),
                a if name.is_none() => name = Some(a.to_owned()),
                a if knobs.is_none() => knobs = Some(Knobs::parse(a).ok_or_else(|| format!("{a:?} are not knobs"))?),
                a => return Err(format!("render: what is {a:?}?\n{USAGE}")),
            }
        }
        let name = name.ok_or(USAGE)?;
        let story = self.story(&name).ok_or_else(|| format!("no story {name:?} (`list` shows them)"))?;
        let knobs = knobs.unwrap_or(story.variants[0]);
        let out = out.unwrap_or_else(|| out_dir().join(&story.name).join(format!("{}.png", knobs.id())));
        self.render(story, &knobs)?.save(&out)?;
        println!("{}", out.display());
        Ok(true)
    }

    fn check(&self, args: &[String]) -> Result<bool, String> {
        let mut update = std::env::var_os("STORIES_UPDATE").is_some_and(|v| v != "0");
        let mut filter = None;
        for arg in args {
            match arg.as_str() {
                "--update" => update = true,
                a if filter.is_none() => filter = Some(a.to_owned()),
                a => return Err(format!("check: what is {a:?}?\n{USAGE}")),
            }
        }
        let jobs: Vec<Job> = self
            .stories
            .iter()
            .filter(|s| filter.as_deref().is_none_or(|f| s.name.contains(f)))
            .flat_map(|s| {
                s.variants.iter().map(|knobs| Job {
                    name: s.name.clone(),
                    knobs: *knobs,
                    threshold: s.threshold,
                    tolerance: s.tolerance,
                })
            })
            .collect();
        if jobs.is_empty() {
            return Err(format!("no stories match {:?}", filter.unwrap_or_default()));
        }
        let failures = out_dir().join("failures");
        let _ = std::fs::remove_dir_all(&failures);
        let golden = &self.golden;
        let results = parallel(&jobs, |job| job.check(golden, update, &failures));
        let mut failed = 0;
        for (job, result) in jobs.iter().zip(results) {
            let label = format!("{} {}", job.name, job.knobs.id());
            match result {
                Ok(note) => println!("ok      {label}{note}"),
                Err(e) => {
                    failed += 1;
                    println!("FAILED  {label}: {e}");
                }
            }
        }
        println!("{} of {} renders {}", jobs.len() - failed, jobs.len(), if update { "written" } else { "match" });
        if failed > 0 && !update {
            println!("what differs is in {}", failures.display());
        }
        Ok(failed == 0)
    }
}

/// One variant of one story to check.
struct Job {
    name: String,
    knobs: Knobs,
    threshold: u8,
    tolerance: f64,
}

impl Job {
    /// Render one variant and compare it (or write it, updating); `Ok` with
    /// a note to show beside it.
    fn check(&self, golden_dir: &Path, update: bool, failures: &Path) -> Result<String, String> {
        let (story, knobs) = (self, &self.knobs);
        let picture = render_in_child(&self.name, knobs)?;
        let golden = golden_dir.join(&self.name).join(format!("{}.png", knobs.id()));
        if update {
            let same = Picture::open(&golden).is_ok_and(|g| picture.compare(&g, 0).passes(0.0));
            if same {
                return Ok(String::new());
            }
            picture.save(&golden)?;
            return Ok(" (written)".into());
        }
        let failed = |picture: &Picture, diff: Option<&Picture>, why: String| {
            let base = failures.join(&story.name);
            picture.save(&base.join(format!("{}.png", knobs.id())))?;
            if let Some(diff) = diff {
                diff.save(&base.join(format!("{}.diff.png", knobs.id())))?;
            }
            Err(why)
        };
        let Ok(expected) = Picture::open(&golden) else {
            return failed(&picture, None, format!("no golden image {} (check --update writes it)", golden.display()));
        };
        match picture.compare(&expected, story.threshold) {
            c if c.passes(story.tolerance) => Ok(match c {
                Comparison::Pixels { differing, .. } => format!(" ({differing} pixels differ, within tolerance)"),
                _ => String::new(),
            }),
            Comparison::Size { actual, golden } => failed(
                &picture,
                None,
                format!("{}×{} pixels, the golden image {}×{}", actual.0, actual.1, golden.0, golden.1),
            ),
            Comparison::Pixels { differing, of, worst, diff } => {
                failed(&picture, Some(&diff), format!("{differing} of {of} pixels differ, by up to {worst}"))
            }
            Comparison::Same => unreachable!("the same always passes"),
        }
    }
}

/// Render the story `name` in `knobs` in a child process of this binary.
fn render_in_child(name: &str, knobs: &Knobs) -> Result<Picture, String> {
    let out = out_dir().join("renders").join(name).join(format!("{}.png", knobs.id()));
    let exe = std::env::current_exe().map_err(|e| format!("this binary: {e}"))?;
    let runtime = out_dir().join("runtime");
    std::fs::create_dir_all(&runtime).map_err(|e| format!("{}: {e}", runtime.display()))?;
    let _ = std::fs::remove_file(&out);
    let mut command = Command::new(exe);
    command.env(CHILD, format!("{name}\n{}\n{}", knobs.id(), out.display()));
    child_env(&mut command, knobs, &runtime);
    let output = command.output().map_err(|e| format!("starting a child: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("the child failed ({}): {}", output.status, stderr.trim()));
    }
    Picture::open(&out)
}

const USAGE: &str = "usage: list | render NAME [KNOBS] [--out FILE] | check [FILTER] [--update]
KNOBS: light@1x, dark@2x, dark-hc@1x, light+e66100@1.5x";

/// Where renders and failures go: `STORIES_OUT`, else `target/stories`.
fn out_dir() -> PathBuf {
    std::env::var_os("STORIES_OUT").map_or_else(|| PathBuf::from("target/stories"), PathBuf::from)
}

/// A child render's environment: the knobs, the null render thread, and
/// none of the desktop the runner was started in.
fn child_env(command: &mut Command, knobs: &Knobs, runtime: &Path) {
    for var in
        ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "SIDESTEP_THEME", "GTK_THEME"]
    {
        command.env_remove(var);
    }
    command.env("XDG_RUNTIME_DIR", runtime);
    command.env("SIDESTEP_BACKEND", "null");
    command.env("SIDESTEP_APPEARANCE", if knobs.appearance.dark { "dark" } else { "light" });
    match knobs.accent {
        Some(rgb) => command.env("SIDESTEP_ACCENT", format!("#{rgb:06x}")),
        None => command.env_remove("SIDESTEP_ACCENT"),
    };
}

/// `f` over `items` on as many threads as there are processors.
fn parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(items.len()).max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<R>>> = items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    *results[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(f(item));
                }
            });
        }
    });
    results.into_iter().map(|r| r.into_inner().unwrap_or_else(|e| e.into_inner()).expect("every item ran")).collect()
}
