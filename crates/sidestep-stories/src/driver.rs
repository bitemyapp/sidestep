//! Running a whole program as a story: its handler under the null render
//! thread, its steps played in turn, and its first window's pixels taken
//! once the last step has shown.

use std::time::{Duration, Instant};

use sidestep_engine::keys::character as ch;
use sidestep_ui::kurbo::Point;
use sidestep_ui::testing;
use sidestep_ui::*;

use crate::Knobs;
use crate::picture::Picture;

/// How long a story may take to settle before it fails.
const DEADLINE: Duration = Duration::from_secs(30);
/// How often the driver looks whether the program has settled.
const POLL: Duration = Duration::from_millis(5);
/// Turns of the loop a step's input takes to land and be drawn: the
/// injected messages are handled on the next turn, and what they
/// invalidated is drawn and shown on the ones after.
const SETTLE_POLLS: u32 = 4;

/// One step of an app story: what it does to the program, given the
/// program's handler (to ask it where things are) and a [`Driver`].
pub(crate) type Step<H> = Box<dyn Fn(&mut H, &mut Driver<'_>)>;

/// What a step uses to act on the program as a person would.
pub struct Driver<'a> {
    cx: &'a mut Cx,
    window: WindowId,
    wait: Duration,
}

impl Driver<'_> {
    /// The program's context, for what the driver doesn't offer.
    pub fn cx(&mut self) -> &mut Cx {
        self.cx
    }

    /// The window the story shows (the program's first).
    pub fn window(&self) -> WindowId {
        self.window
    }

    /// The pointer moving to `at` (points, in the window).
    pub fn move_to(&mut self, at: Point) {
        testing::inject_motion(self.cx, self.window, at);
    }

    /// The left button clicked at `at`.
    pub fn click(&mut self, at: Point) {
        self.click_with(at, PointerButton::Left, 1, Modifiers::NONE);
    }

    /// `button` clicked at `at` as click `clicks` of a series, with
    /// `modifiers` held.
    pub fn click_with(&mut self, at: Point, button: PointerButton, clicks: u32, modifiers: Modifiers) {
        testing::inject_motion_with(self.cx, self.window, at, modifiers);
        testing::inject_button_with(self.cx, self.window, at, button, true, clicks, modifiers);
        testing::inject_button_with(self.cx, self.window, at, button, false, clicks, modifiers);
    }

    /// `text` typed a character at a time.
    pub fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            let s = c.to_string();
            let shift = if c.is_uppercase() { Modifiers::SHIFT } else { Modifiers::NONE };
            self.press(&s, shift);
        }
    }

    /// `key` pressed and released.
    pub fn key(&mut self, key: NamedKey) {
        self.key_with(key, Modifiers::NONE);
    }

    /// `key` pressed and released with `modifiers` held.
    pub fn key_with(&mut self, key: NamedKey, modifiers: Modifiers) {
        let c = named(key).unwrap_or_else(|| panic!("sidestep-stories: no way yet to type {key:?}"));
        self.press(&char::from_u32(c).expect("a key's character").to_string(), modifiers);
    }

    /// A shortcut: `c` with `modifiers` (Control-C is `shortcut('c',
    /// Modifiers::CONTROL)`).
    pub fn shortcut(&mut self, c: char, modifiers: Modifiers) {
        self.press(&c.to_string(), modifiers);
    }

    /// Wait at least `time` after this step before the next one, for what
    /// runs on timers.
    pub fn wait(&mut self, time: Duration) {
        self.wait = self.wait.max(time);
    }

    fn press(&mut self, characters: &str, modifiers: Modifiers) {
        for pressed in [true, false] {
            testing::inject_key(self.cx, self.window, 0, characters, characters, modifiers, pressed);
        }
    }
}

/// The character AppKit, and so the render thread, types `key` with.
fn named(key: NamedKey) -> Option<u32> {
    Some(match key {
        NamedKey::Enter => ch::CARRIAGE_RETURN,
        NamedKey::Tab => ch::TAB,
        NamedKey::Backspace => ch::DELETE,
        NamedKey::Escape => ch::ESCAPE,
        NamedKey::Delete => ch::DELETE_FORWARD,
        NamedKey::ArrowUp => ch::UP_ARROW,
        NamedKey::ArrowDown => ch::DOWN_ARROW,
        NamedKey::ArrowLeft => ch::LEFT_ARROW,
        NamedKey::ArrowRight => ch::RIGHT_ARROW,
        NamedKey::Home => ch::HOME,
        NamedKey::End => ch::END,
        NamedKey::PageUp => ch::PAGE_UP,
        NamedKey::PageDown => ch::PAGE_DOWN,
        _ => return None,
    })
}

/// Run `program` as a story in `knobs`, play `steps`, and give its first
/// window's pixels. Call once in a process, in a child the runner starts
/// with the environment the knobs need (see `crate::child_env`).
pub(crate) fn run<H: Handler>(program: H, steps: &[Step<H>], knobs: &Knobs) -> Result<Picture, String> {
    if knobs.scale.fract() != 0.0 || knobs.scale < 1.0 {
        return Err(format!("a program's window shows at a whole scale, not {}", knobs.scale));
    }
    testing::use_null_backend();
    testing::use_null_backend_scale(knobs.scale as u32);
    testing::capture_pixels(true);
    let story = Story {
        program,
        steps,
        appearance: knobs.appearance,
        window: None,
        poll: None,
        step: 0,
        quiet: 0,
        draws: 0,
        not_before: None,
        started: Instant::now(),
        result: None,
    };
    let story = App::new().run(story).map_err(|e| e.to_string())?;
    story.result.unwrap_or_else(|| Err("the program quit before its story was taken".into()))
}

struct Story<'s, H> {
    program: H,
    steps: &'s [Step<H>],
    appearance: Appearance,
    window: Option<WindowId>,
    poll: Option<TimerId>,
    /// The next step to play.
    step: usize,
    /// Polls in a row with the render thread idle and nothing drawn.
    quiet: u32,
    draws: u32,
    not_before: Option<Instant>,
    started: Instant,
    result: Option<Result<Picture, String>>,
}

impl<H: Handler> Story<'_, H> {
    fn finish(&mut self, cx: &mut Cx, result: Result<Picture, String>) {
        self.result = Some(result);
        cx.quit();
    }

    /// Play the next step, or take the picture, once the program has
    /// settled.
    fn poll(&mut self, cx: &mut Cx) {
        if self.started.elapsed() > DEADLINE {
            let at = format!("step {} of {}", self.step, self.steps.len());
            return self.finish(cx, Err(format!("the program did not settle within {DEADLINE:?} ({at})")));
        }
        let Some(window) = self.window.or_else(|| cx.windows().first().copied()) else { return };
        self.window = Some(window);
        if testing::render_idle() {
            self.quiet += 1;
        } else {
            self.quiet = 0;
        }
        let waited = self.not_before.is_none_or(|t| Instant::now() >= t);
        if self.draws == 0 || self.quiet < SETTLE_POLLS || !waited {
            return;
        }
        let Some(step) = self.steps.get(self.step) else {
            let picture = testing::window_pixels(window)
                .map(|(w, h, px)| Picture::from_premultiplied(w, h, px))
                .ok_or_else(|| "the window kept no pixels".to_owned());
            return self.finish(cx, picture);
        };
        let mut driver = Driver { cx, window, wait: Duration::ZERO };
        step(&mut self.program, &mut driver);
        let wait = driver.wait;
        self.not_before = (!wait.is_zero()).then(|| Instant::now() + wait);
        self.step += 1;
        self.quiet = 0;
    }
}

impl<H: Handler> Handler for Story<'_, H> {
    fn launched(&mut self, cx: &mut Cx) {
        // The environment set light or dark; high contrast has no variable.
        if self.appearance.high_contrast {
            sidestep_engine::settings::change_for_testing(self.appearance.dark, true);
        }
        self.program.launched(cx);
        self.poll = Some(cx.set_repeating_timer(POLL));
    }

    fn window_event(&mut self, cx: &mut Cx, window: WindowId, event: WindowEvent) {
        self.program.window_event(cx, window, event);
    }

    fn draw(&mut self, cx: &mut Cx, window: WindowId, canvas: &mut Canvas) {
        self.draws += 1;
        self.quiet = 0;
        self.program.draw(cx, window, canvas);
    }

    fn timer(&mut self, cx: &mut Cx, timer: TimerId) {
        if Some(timer) == self.poll {
            if self.result.is_none() {
                self.poll(cx);
            }
        } else {
            self.program.timer(cx, timer);
        }
    }

    fn woken(&mut self, cx: &mut Cx) {
        self.program.woken(cx);
    }

    fn appearance_changed(&mut self, cx: &mut Cx, appearance: Appearance) {
        self.program.appearance_changed(cx, appearance);
    }

    fn outputs_changed(&mut self, cx: &mut Cx) {
        self.program.outputs_changed(cx);
    }

    fn session_locked(&mut self, cx: &mut Cx) {
        self.program.session_locked(cx);
    }

    fn activation_token(&mut self, cx: &mut Cx, request: ActivationRequest, token: Option<String>) {
        self.program.activation_token(cx, request, token);
    }

    fn session_lock_finished(&mut self, cx: &mut Cx) {
        self.program.session_lock_finished(cx);
    }

    fn drag_moved(&mut self, cx: &mut Cx, window: WindowId, drag: &Drag) -> DropResponse {
        self.program.drag_moved(cx, window, drag)
    }

    fn drag_left(&mut self, cx: &mut Cx, window: WindowId) {
        self.program.drag_left(cx, window);
    }

    fn dropped(&mut self, cx: &mut Cx, window: WindowId, drag: &Drag, response: &DropResponse) -> bool {
        self.program.dropped(cx, window, drag, response)
    }

    fn provide_drag_data(&mut self, cx: &mut Cx, drag: DragId, mime: &str) -> Option<Vec<u8>> {
        self.program.provide_drag_data(cx, drag, mime)
    }

    fn drag_ended(&mut self, cx: &mut Cx, drag: DragId, action: DropAction) {
        self.program.drag_ended(cx, drag, action);
    }
}
