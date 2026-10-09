//! The application: its event loop, the [`Handler`] a program gives it,
//! and [`Cx`], what a handler is given to act with.
//!
//! The loop runs on the thread that calls [`App::run`]; the handler and
//! everything it touches stay there. Each turn it handles what the render
//! thread sent (input, configures, frames), wakes from a [`Proxy`], the
//! desktop's settings changing, timers that came due and touchpad
//! coasting, then has each window that needs it draw (a window draws only
//! once the render thread has shown its last frame, so drawing keeps pace
//! with the screen), and sleeps until something wakes it. An idle program
//! sleeps with nothing armed.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use kurbo::{Point, Rect, Size, Vec2};
use sidestep_engine::backend::{self, Backend};
use sidestep_engine::clipboard::{self, Contents, Source};
use sidestep_engine::protocol::{FromRender, Target, TitleText, ToRender};
use sidestep_engine::{momentum, outputs, settings};

use crate::canvas::Canvas;
use crate::color::{Appearance, Color, SystemColor};
use crate::event::{Ime, KeyEvent, Modifiers, PointerButton, ScrollDelta, ScrollPhase, WindowEvent, WindowState};
use crate::text::{Font, TextStyle};
use crate::window::{Background, Window, WindowData, WindowId, WindowOptions, send};

/// What a program does with its application's events. Every method but
/// [`draw`](Handler::draw) has a default that does nothing.
pub trait Handler {
    /// The application started: open the first windows.
    fn launched(&mut self, cx: &mut Cx) {
        let _ = cx;
    }

    /// Something happened to `window` or in it.
    fn window_event(&mut self, cx: &mut Cx, window: WindowId, event: WindowEvent) {
        let _ = (cx, window, event);
    }

    /// Draw `window`: the parts of it the canvas's damage names, which
    /// start out cleared to the window's background. Called when the
    /// window was invalidated, resized or first shown, once the render
    /// thread is ready for its next frame.
    fn draw(&mut self, cx: &mut Cx, window: WindowId, canvas: &mut Canvas);

    /// A timer came due.
    fn timer(&mut self, cx: &mut Cx, timer: TimerId) {
        let _ = (cx, timer);
    }

    /// A [`Proxy`] woke the loop.
    fn woken(&mut self, cx: &mut Cx) {
        let _ = cx;
    }

    /// The desktop went light or dark, changed its contrast or accent
    /// color. Every window redraws.
    fn appearance_changed(&mut self, cx: &mut Cx, appearance: Appearance) {
        let _ = (cx, appearance);
    }

    /// The outputs (screens) came, went or changed, or were described for
    /// the first time ([`Cx::outputs`]).
    fn outputs_changed(&mut self, cx: &mut Cx) {
        let _ = cx;
    }

    /// A drag entered `window` or moved over it (or waits there, if the
    /// last answer asked to be asked periodically): what dropping it there
    /// would do. The default, [`DropResponse::REFUSE`], refuses it.
    fn drag_moved(&mut self, cx: &mut Cx, window: WindowId, drag: &Drag) -> DropResponse {
        let _ = (cx, window, drag);
        DropResponse::REFUSE
    }

    /// The drag left `window`, or was cancelled.
    fn drag_left(&mut self, cx: &mut Cx, window: WindowId) {
        let _ = (cx, window);
    }

    /// The drag was dropped on `window`, to what
    /// [`drag_moved`](Handler::drag_moved) last answered (its action, and
    /// the MIME type taken: its `mime` is always set here): whether the
    /// window took it. Its data beyond its URLs is read with
    /// [`Cx::drag_data`].
    fn dropped(&mut self, cx: &mut Cx, window: WindowId, drag: &Drag, response: &DropResponse) -> bool {
        let _ = (cx, window, drag, response);
        false
    }
}

/// A drag over a window.
#[derive(Clone, Debug, PartialEq)]
pub struct Drag {
    /// Where it is, in the window's content points.
    pub position: Point,
    /// The MIME types it offers.
    pub mimes: Vec<String>,
    /// The URLs it carries (files, links), when it offers a list of them.
    pub urls: Vec<String>,
    /// What its source allows.
    pub copy_allowed: bool,
    pub move_allowed: bool,
}

/// What dropping a drag does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DropAction {
    #[default]
    None,
    Copy,
    Move,
}

/// What a window would do with a drag dropped where it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct DropResponse {
    /// What the drop does; [`DropAction::None`] refuses it. An action the
    /// source doesn't allow becomes the other, if the source allows that.
    pub action: DropAction,
    /// The MIME type to take the data as, one of those the drag offers;
    /// none (or one it doesn't offer) for the toolkit's choice: its URL
    /// list if it has URLs, else text, else the first type it offers.
    pub mime: Option<String>,
    /// Ask again periodically while the drag waits where it is, as a view
    /// that scrolls while a drag waits at its edge needs.
    pub periodic: bool,
}

impl DropResponse {
    pub const REFUSE: DropResponse = DropResponse { action: DropAction::None, mime: None, periodic: false };

    pub fn copy() -> DropResponse {
        DropAction::Copy.into()
    }

    pub fn moving() -> DropResponse {
        DropAction::Move.into()
    }

    /// Take the data as `mime`.
    pub fn with_mime(mut self, mime: impl Into<String>) -> DropResponse {
        self.mime = Some(mime.into());
        self
    }

    /// Be asked again periodically while the drag waits.
    pub fn periodic(mut self) -> DropResponse {
        self.periodic = true;
        self
    }
}

impl From<DropAction> for DropResponse {
    fn from(action: DropAction) -> DropResponse {
        DropResponse { action, mime: None, periodic: false }
    }
}

/// A timer, as [`Cx::set_timer`] returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimerId(u64);

/// Why [`App::run`] stopped without being asked to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The render thread stopped: the Wayland compositor couldn't be
    /// reached, or the connection to it failed.
    DisplayLost,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::DisplayLost => {
                f.write_str("the Wayland compositor couldn't be reached, or the connection to it failed")
            }
        }
    }
}

impl std::error::Error for Error {}

/// What wakes the loop: the render thread's messages, proxies and the
/// settings thread.
#[derive(Default)]
struct Wake {
    signalled: Mutex<bool>,
    cond: Condvar,
    /// A proxy asked for the handler's `woken`.
    woken: AtomicBool,
}

impl Wake {
    fn wake(&self) {
        *self.signalled.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cond.notify_one();
    }

    /// Sleep until woken, or until `until`.
    fn wait(&self, until: Option<Instant>) {
        let mut signalled = self.signalled.lock().unwrap_or_else(|e| e.into_inner());
        while !*signalled {
            match until {
                None => signalled = self.cond.wait(signalled).unwrap_or_else(|e| e.into_inner()),
                Some(at) => {
                    let left = at.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return;
                    }
                    signalled = self.cond.wait_timeout(signalled, left).unwrap_or_else(|e| e.into_inner()).0;
                }
            }
        }
        *signalled = false;
    }
}

/// Wakes an application's loop from any thread, for its handler's
/// [`woken`](Handler::woken): what a worker thread uses to say its work is
/// done (with a channel of the program's own for the results).
#[derive(Clone)]
pub struct Proxy {
    wake: Arc<Wake>,
}

impl Proxy {
    pub fn wake(&self) {
        self.wake.woken.store(true, Ordering::Release);
        self.wake.wake();
    }
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Proxy")
    }
}

/// An application: [`App::new`], then [`App::run`] with a [`Handler`].
#[derive(Debug)]
pub struct App {
    quit_when_last_window_closes: bool,
    momentum_scrolling: bool,
}

impl Default for App {
    fn default() -> App {
        App::new()
    }
}

/// An application runs: a process has one render thread at a time.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Lets the next application run once this one stops, however it stops.
struct Running;

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::Release);
    }
}

impl App {
    pub fn new() -> App {
        App { quit_when_last_window_closes: true, momentum_scrolling: true }
    }

    /// Stop running when the last open window closes (the default), or
    /// only when the handler calls [`Cx::quit`].
    pub fn quit_when_last_window_closes(mut self, quit: bool) -> App {
        self.quit_when_last_window_closes = quit;
        self
    }

    /// Coast after touchpad flicks (the default), or leave it to the
    /// program (see [`WindowEvent::Scroll`]'s `momentum`).
    pub fn momentum_scrolling(mut self, coast: bool) -> App {
        self.momentum_scrolling = coast;
        self
    }

    /// Run until the handler quits (or the last window closes), and give
    /// the handler back. When it returns, its windows are going from the
    /// screen and its render thread is stopping; another application may
    /// run after it.
    ///
    /// # Panics
    ///
    /// If another application is running in this process (one with this
    /// toolkit, on another thread).
    pub fn run<H: Handler>(self, mut handler: H) -> Result<H, Error> {
        assert!(!RUNNING.swap(true, Ordering::AcqRel), "sidestep-ui: an application is already running");
        let _running = Running;
        let wake = Arc::new(Wake::default());
        let changed = wake.clone();
        settings::on_change(move || changed.wake());
        settings::start();
        let render = wake.clone();
        let backend = backend::start(Arc::new(move || render.wake()));
        let mut cx = Cx {
            backend,
            wake,
            inbox: VecDeque::new(),
            synthetic: VecDeque::new(),
            windows: BTreeMap::new(),
            showings: HashMap::new(),
            next_window: 1,
            next_showing: RESHOWN,
            timers: BTreeMap::new(),
            timer_due: HashMap::new(),
            next_timer: 1,
            quit: false,
            quit_when_last_window_closes: self.quit_when_last_window_closes,
            momentum_scrolling: self.momentum_scrolling,
            coast: None,
            opened_any: false,
            clipboard: None,
            drags: HashMap::new(),
            looks: (Appearance::system(), Appearance::system().color(SystemColor::Accent)),
        };
        handler.launched(&mut cx);
        let result = cx.run(&mut handler);
        cx.close_all();
        result.map(|()| handler)
    }
}

/// Where the render thread's ids for showings after a window's first
/// start: a window's first showing has the window's own number, so the
/// two never meet.
const RESHOWN: u32 = 1 << 31;

/// A drag over one of the windows, and what the handler last answered.
struct DragState {
    window: WindowId,
    drag: Drag,
    response: DropResponse,
}

/// A touchpad flick coasting over a window.
struct Coasting {
    window: WindowId,
    position: Point,
    modifiers: Modifiers,
    inverted: bool,
    physics: momentum::Coast,
}

/// The application as a handler acts on it: windows, timers, the
/// clipboard, quitting.
pub struct Cx {
    backend: Backend,
    wake: Arc<Wake>,
    /// Messages from the render thread not handled yet, oldest first.
    inbox: VecDeque<FromRender>,
    /// Events the toolkit makes itself (a window hidden loses the
    /// keyboard and the pointer), told before the next messages.
    synthetic: VecDeque<(WindowId, WindowEvent)>,
    windows: BTreeMap<WindowId, WindowData>,
    /// The window each of the render thread's showings is.
    showings: HashMap<u32, WindowId>,
    next_window: u32,
    next_showing: u32,
    /// Timers by when they're due (and their id, for order), with their
    /// period if they repeat; and when each is due, by id.
    timers: BTreeMap<(Instant, u64), Option<Duration>>,
    timer_due: HashMap<u64, Instant>,
    next_timer: u64,
    quit: bool,
    quit_when_last_window_closes: bool,
    momentum_scrolling: bool,
    coast: Option<Coasting>,
    opened_any: bool,
    /// The text the program last put on the clipboard, with the change it
    /// made.
    clipboard: Option<(isize, String)>,
    drags: HashMap<u64, DragState>,
    /// The appearance and accent windows last drew in.
    looks: (Appearance, Color),
}

impl Cx {
    // Windows.

    /// Open a window. It shows once the compositor configures it; the
    /// handler hears its size then, and draws it. (A window opened hidden,
    /// [`WindowOptions::visible`], waits for [`show_window`](Cx::show_window).)
    pub fn open_window(&mut self, options: WindowOptions) -> WindowId {
        let id = WindowId(self.next_window);
        self.next_window += 1;
        self.opened_any = true;
        self.windows.insert(id, WindowData::new(id, &options));
        if options.is_visible() {
            self.show_window(id);
        }
        id
    }

    /// An open window, shown or hidden, or `None` once it's closed.
    pub fn window(&mut self, id: WindowId) -> Option<Window<'_>> {
        let data = self.windows.get_mut(&id)?;
        Some(Window { data, tx: &self.backend.tx })
    }

    /// The open windows, shown or hidden, oldest first.
    pub fn windows(&self) -> Vec<WindowId> {
        self.windows.keys().copied().collect()
    }

    /// Show a hidden window again, as it was: its title, style and size
    /// (the compositor configures it anew, and it draws whole). A popup
    /// shows only while the window it's part of does.
    pub fn show_window(&mut self, id: WindowId) {
        let Some(w) = self.windows.get(&id) else { return };
        if w.showing.is_some() {
            return;
        }
        let parent = w.popup_of().or(w.transient_for());
        let parent_showing = parent.and_then(|p| self.windows.get(&p)).and_then(|p| p.showing);
        if w.popup_of().is_some() && parent_showing.is_none() {
            return;
        }
        // The first showing has the window's number; later ones count on
        // from `RESHOWN`.
        let showing = if self.shown_before(id) {
            let s = self.next_showing;
            self.next_showing += 1;
            s
        } else {
            id.0
        };
        let Some(w) = self.windows.get_mut(&id) else { return };
        w.show(showing, parent_showing, &self.backend.tx);
        w.shown_before = true;
        self.showings.insert(showing, id);
        // Windows kept over this one are kept over its new showing.
        let children: Vec<u32> =
            self.windows.values().filter(|c| c.transient_for() == Some(id)).filter_map(|c| c.showing).collect();
        for child in children {
            send(&self.backend.tx, ToRender::SetParent { window: child, parent: Some(showing) });
        }
    }

    fn shown_before(&self, id: WindowId) -> bool {
        self.windows.get(&id).is_some_and(|w| w.shown_before)
    }

    /// Take a window off the screen, keeping it (and its id) for
    /// [`show_window`](Cx::show_window). Its popups close; if it had the
    /// keyboard or the pointer, the handler hears that it lost them.
    pub fn hide_window(&mut self, id: WindowId) {
        if self.windows.get(&id).is_none_or(|w| w.showing.is_none()) {
            return;
        }
        let popups: Vec<WindowId> = self.windows.values().filter(|w| w.popup_of() == Some(id)).map(|w| w.id).collect();
        for popup in popups {
            self.close_window(popup);
        }
        self.forget_showing(id);
        let Some(w) = self.windows.get_mut(&id) else { return };
        if w.focused {
            self.synthetic.push_back((id, WindowEvent::Focused(false)));
        }
        if w.pointer_inside {
            self.synthetic.push_back((id, WindowEvent::PointerLeft));
        }
        w.hide(&self.backend.tx);
    }

    /// The window goes from the render thread: what it still says of the
    /// showing finds nothing, and coasting and drags over it end.
    fn forget_showing(&mut self, id: WindowId) {
        self.showings.retain(|_, w| *w != id);
        self.drags.retain(|_, d| d.window != id);
        if self.coast.as_ref().is_some_and(|c| c.window == id) {
            self.coast = None;
        }
    }

    /// Close a window, and the popups over it first (a popup can't outlive
    /// its parent): it goes from the screen, and its id means nothing from
    /// now on.
    pub fn close_window(&mut self, id: WindowId) {
        let popups: Vec<WindowId> = self.windows.values().filter(|w| w.popup_of() == Some(id)).map(|w| w.id).collect();
        for popup in popups {
            self.close_window(popup);
        }
        if let Some(mut w) = self.windows.remove(&id) {
            w.hide(&self.backend.tx);
            self.forget_showing(id);
        }
    }

    /// Stop the application: [`App::run`] returns after the current event.
    pub fn quit(&mut self) {
        self.quit = true;
    }

    // Timers.

    /// Call the handler's [`timer`](Handler::timer) once, `after` from now.
    pub fn set_timer(&mut self, after: Duration) -> TimerId {
        self.add_timer(Instant::now() + after, None)
    }

    /// Call the handler's [`timer`](Handler::timer) every `period` (at
    /// least a millisecond), until cancelled. A timer late by more than a
    /// period skips the calls it missed.
    pub fn set_repeating_timer(&mut self, period: Duration) -> TimerId {
        let period = period.max(Duration::from_millis(1));
        self.add_timer(Instant::now() + period, Some(period))
    }

    pub fn cancel_timer(&mut self, timer: TimerId) {
        if let Some(at) = self.timer_due.remove(&timer.0) {
            self.timers.remove(&(at, timer.0));
        }
    }

    fn add_timer(&mut self, at: Instant, period: Option<Duration>) -> TimerId {
        let id = self.next_timer;
        self.next_timer += 1;
        self.timers.insert((at, id), period);
        self.timer_due.insert(id, at);
        TimerId(id)
    }

    /// Queue `msg` as if the render thread had sent it, after what it
    /// did send (`testing`).
    pub(crate) fn inject(&mut self, msg: FromRender) {
        self.inbox.push_back(msg);
    }

    /// The render thread's id for `window`'s current showing (`testing`).
    pub(crate) fn showing_of(&self, window: WindowId) -> Option<u32> {
        self.windows.get(&window)?.showing
    }

    /// A handle that wakes this loop from other threads.
    pub fn proxy(&self) -> Proxy {
        Proxy { wake: self.wake.clone() }
    }

    // The desktop.

    /// The desktop's appearance, as last told.
    pub fn appearance(&self) -> Appearance {
        Appearance::system()
    }

    /// `color` in the desktop's appearance.
    pub fn system_color(&self, color: SystemColor) -> Color {
        Appearance::system().color(color)
    }

    /// The outputs (screens), as the compositor last described them; none
    /// before it first did, a round trip or two after the application
    /// starts (the handler hears [`outputs_changed`](Handler::outputs_changed)
    /// then), and none without a display. This never waits.
    pub fn outputs(&self) -> Vec<Output> {
        outputs::latest().map(|all| all.iter().map(Output::of).collect()).unwrap_or_default()
    }

    /// Hide the pointer over every window, until it moves if
    /// `until_moved`.
    pub fn hide_cursor(&mut self, until_moved: bool) {
        send(&self.backend.tx, ToRender::HideCursor { hidden: true, until_moved });
    }

    pub fn show_cursor(&mut self) {
        send(&self.backend.tx, ToRender::HideCursor { hidden: false, until_moved: false });
    }

    // The clipboard.

    /// Put `text` on the clipboard, for this program and others. It takes
    /// once one of the windows has had input (Wayland's rule).
    pub fn set_clipboard_text(&mut self, text: &str) {
        let shared = clipboard::shared();
        let generation = shared.bump();
        self.clipboard = Some((generation, text.to_owned()));
        let bytes: Arc<[u8]> = Arc::from(text.as_bytes());
        let items = clipboard::TEXT_MIMES.iter().map(|m| ((*m).to_owned(), Some(bytes.clone()))).collect();
        shared.offer(Some(Contents { items }));
    }

    /// The clipboard's text: this program's, or another's, which may take
    /// a moment to arrive (another program sends it through a pipe; the
    /// wait is bounded, and a program that never answers costs it once).
    pub fn clipboard_text(&self) -> Option<String> {
        let shared = clipboard::shared();
        match &self.clipboard {
            Some((generation, text)) if shared.change_count() == *generation => Some(text.clone()),
            _ => shared.foreign_text(),
        }
    }

    /// The data a drag over one of the windows offers as `mime`, while the
    /// handler hears of it (most useful in [`Handler::dropped`]).
    pub fn drag_data(&self, mime: &str) -> Option<Vec<u8>> {
        clipboard::shared_for(Source::Drag).read_offer(mime, None).map(|d| d.to_vec())
    }

    // The loop.

    fn run(&mut self, handler: &mut impl Handler) -> Result<(), Error> {
        loop {
            loop {
                match self.backend.rx.try_recv() {
                    Ok(msg) => self.inbox.push_back(msg),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Err(Error::DisplayLost),
                }
            }
            loop {
                if let Some((window, event)) = self.synthetic.pop_front() {
                    self.tell(handler, window, event);
                } else if let Some(msg) = self.next_message() {
                    self.handle(handler, msg);
                } else {
                    break;
                }
                if self.done() {
                    return Ok(());
                }
            }
            if self.wake.woken.swap(false, Ordering::AcqRel) {
                handler.woken(self);
            }
            if settings::take_changed() {
                // Only a new look or accent changes how windows draw.
                let now = (Appearance::system(), Appearance::system().color(SystemColor::Accent));
                if std::mem::replace(&mut self.looks, now) != now {
                    for w in self.windows.values_mut() {
                        w.invalidate_all();
                    }
                    handler.appearance_changed(self, now.0);
                }
            }
            self.fire_timers(handler);
            // Coasting no frame moved on for a while moves anyway.
            if self.coast.as_ref().is_some_and(|c| Instant::now() >= c.physics.deadline()) {
                self.coast_step(handler, Instant::now());
            }
            if self.done() {
                return Ok(());
            }
            self.display(handler);
            let pending = !self.inbox.is_empty() || !self.synthetic.is_empty();
            if pending || self.wake.woken.load(Ordering::Acquire) {
                continue;
            }
            // A first frame waits (briefly) for the desktop's appearance.
            let first_frame = settings::deadline().filter(|_| self.windows.values().any(|w| w.wants_pass()));
            let timer = self.timers.keys().next().map(|(at, _)| *at);
            let coast = self.coast.as_ref().map(|c| c.physics.deadline());
            let until = [first_frame, timer, coast].into_iter().flatten().min();
            self.wake.wait(until);
        }
    }

    fn done(&self) -> bool {
        self.quit || (self.quit_when_last_window_closes && self.opened_any && self.windows.is_empty())
    }

    fn close_all(&mut self) {
        for id in self.windows() {
            self.close_window(id);
        }
    }

    /// The next message worth handling: a move followed by another move
    /// of the same window is dropped, and so is a key repeat followed by
    /// any newer key message for its window, so a busy loop catches up
    /// rather than falling behind.
    fn next_message(&mut self) -> Option<FromRender> {
        loop {
            let msg = self.inbox.pop_front()?;
            let superseded = match &msg {
                FromRender::Motion { window, .. } => {
                    matches!(self.inbox.front(), Some(FromRender::Motion { window: next, .. }) if next == window)
                }
                FromRender::Key { window, key } if key.repeat => {
                    self.inbox.iter().any(|m| matches!(m, FromRender::Key { window: w, .. } if w == window))
                }
                _ => false,
            };
            if !superseded {
                return Some(msg);
            }
        }
    }

    fn fire_timers(&mut self, handler: &mut impl Handler) {
        let now = Instant::now();
        while let Some((&(at, id), &period)) = self.timers.first_key_value() {
            if at > now {
                break;
            }
            self.timers.remove(&(at, id));
            self.timer_due.remove(&id);
            if let Some(period) = period {
                // Keep the phase; skip what was missed.
                let mut next = at + period;
                if next <= now {
                    let behind = now.duration_since(at).as_nanos() / period.as_nanos().max(1);
                    next = at + period * (u32::try_from(behind).unwrap_or(u32::MAX).saturating_add(1));
                }
                self.timers.insert((next, id), Some(period));
                self.timer_due.insert(id, next);
            }
            handler.timer(self, TimerId(id));
            if self.done() {
                return;
            }
        }
    }

    /// Tell the handler, unless the window closed meanwhile.
    fn tell(&mut self, handler: &mut impl Handler, window: WindowId, event: WindowEvent) {
        if self.windows.contains_key(&window) {
            handler.window_event(self, window, event);
        }
    }

    // Coasting (`sidestep_engine::momentum`, which AppKit shares).

    /// Stop coasting, telling the window it ended if it began.
    fn stop_coasting(&mut self, handler: &mut impl Handler) {
        let Some(c) = self.coast.take() else { return };
        if c.physics.began {
            let event = coast_event(&c, (0.0, 0.0), ScrollPhase::Ended);
            self.tell(handler, c.window, event);
        }
    }

    /// Coast on to `now`.
    fn coast_step(&mut self, handler: &mut impl Handler, now: Instant) {
        let Some(c) = self.coast.as_mut() else { return };
        let step = c.physics.step(now);
        let phase = if step.first { ScrollPhase::Began } else { ScrollPhase::Changed };
        let (window, event) = (c.window, coast_event(c, step.delta, phase));
        self.tell(handler, window, event);
        if step.slow {
            self.stop_coasting(handler);
        }
    }

    fn handle(&mut self, handler: &mut impl Handler, msg: FromRender) {
        // The window a showing is; what's said of a showing that went
        // (hidden, closed) finds nothing, but drags are answered.
        let of = |cx: &Cx, showing: u32| cx.showings.get(&showing).copied();
        match msg {
            FromRender::Configure { window, width, height, scale, titlebar, state } => {
                let Some(window) = of(self, window) else { return };
                let Some(w) = self.windows.get_mut(&window) else { return };
                let first = !std::mem::replace(&mut w.configured, true);
                let size = Size::new(f64::from(width), f64::from(height));
                let resized = first || w.size != size;
                let rescaled = first || w.scale != scale;
                let state = WindowState::from_protocol(state);
                let restated = w.state != state;
                (w.size, w.scale, w.state) = (size, scale, state);
                if w.titlebar != titlebar {
                    w.titlebar = titlebar;
                    w.title_dirty = true;
                }
                if resized || rescaled {
                    // The render thread made a new canvas: draw it all, now.
                    w.frame_pending = false;
                    w.invalidate_all();
                }
                if resized {
                    self.tell(handler, window, WindowEvent::Resized(size));
                }
                if rescaled {
                    self.tell(handler, window, WindowEvent::ScaleChanged(scale));
                }
                if restated {
                    self.tell(handler, window, WindowEvent::StateChanged(state));
                }
            }
            FromRender::Frame { window } => {
                let Some(window) = of(self, window) else { return };
                if let Some(w) = self.windows.get_mut(&window) {
                    w.frame_pending = false;
                }
                if self.coast.as_ref().is_some_and(|c| c.window == window) {
                    self.coast_step(handler, Instant::now());
                }
            }
            FromRender::Focus { window, focused } => {
                let Some(window) = of(self, window) else { return };
                if let Some(w) = self.windows.get_mut(&window) {
                    w.focused = focused;
                    self.tell(handler, window, WindowEvent::Focused(focused));
                }
            }
            FromRender::Key { window, key } => {
                let Some(window) = of(self, window) else { return };
                self.tell(handler, window, WindowEvent::Key(KeyEvent::from_protocol(key)));
            }
            FromRender::Modifiers { window, modifiers, .. } => {
                let Some(window) = of(self, window) else { return };
                self.tell(handler, window, WindowEvent::ModifiersChanged(Modifiers::from_flags(modifiers)));
            }
            FromRender::Enter { window, x, y } => {
                let Some(window) = of(self, window) else { return };
                if let Some(w) = self.windows.get_mut(&window) {
                    w.pointer_inside = true;
                }
                self.tell(handler, window, WindowEvent::PointerEntered(Point::new(x, y)));
            }
            FromRender::Leave { window } => {
                let Some(window) = of(self, window) else { return };
                if let Some(w) = self.windows.get_mut(&window) {
                    w.pointer_inside = false;
                }
                self.tell(handler, window, WindowEvent::PointerLeft);
            }
            FromRender::Motion { window, x, y, modifiers } => {
                let Some(window) = of(self, window) else { return };
                let event = WindowEvent::PointerMoved {
                    position: Point::new(x, y),
                    modifiers: Modifiers::from_flags(modifiers),
                };
                self.tell(handler, window, event);
            }
            FromRender::Button { window, x, y, button, pressed, clicks, modifiers, activating } => {
                let Some(window) = of(self, window) else { return };
                // A press stops coasting.
                if pressed {
                    self.stop_coasting(handler);
                }
                let event = WindowEvent::PointerButton {
                    position: Point::new(x, y),
                    button: PointerButton::from_protocol(button),
                    pressed,
                    clicks,
                    modifiers: Modifiers::from_flags(modifiers),
                    activating,
                };
                self.tell(handler, window, event);
            }
            FromRender::Scroll { window, x, y, dx, dy, wheel, modifiers, phase, velocity, inverted } => {
                let Some(window) = of(self, window) else { return };
                // Scrolling again stops coasting.
                self.stop_coasting(handler);
                let (position, phase, modifiers) =
                    (Point::new(x, y), ScrollPhase::from_protocol(phase), Modifiers::from_flags(modifiers));
                let delta = Vec2::new(dx, dy);
                let event = WindowEvent::Scroll {
                    position,
                    delta: if wheel { ScrollDelta::Lines(delta) } else { ScrollDelta::Points(delta) },
                    phase,
                    momentum: ScrollPhase::None,
                    velocity: Vec2::new(velocity.0, velocity.1),
                    inverted,
                    modifiers,
                };
                self.tell(handler, window, event);
                // A flick ending fast coasts on.
                if phase == ScrollPhase::Ended
                    && !wheel
                    && self.momentum_scrolling
                    && self.windows.contains_key(&window)
                {
                    self.coast = momentum::Coast::start(velocity, Instant::now()).map(|physics| Coasting {
                        window,
                        position,
                        modifiers,
                        inverted,
                        physics,
                    });
                }
            }
            FromRender::Pinch { window, x, y, phase, magnification, rotation, modifiers } => {
                let Some(window) = of(self, window) else { return };
                let event = WindowEvent::Pinch {
                    position: Point::new(x, y),
                    phase: ScrollPhase::from_protocol(phase),
                    magnification,
                    rotation,
                    modifiers: Modifiers::from_flags(modifiers),
                };
                self.tell(handler, window, event);
            }
            FromRender::CloseRequested { window } => {
                let Some(window) = of(self, window) else { return };
                self.tell(handler, window, WindowEvent::CloseRequested);
                if self.windows.get(&window).is_some_and(|w| w.close_on_request) {
                    self.close_window(window);
                }
            }
            FromRender::PopupDone { window } => {
                let Some(window) = of(self, window) else { return };
                self.tell(handler, window, WindowEvent::PopupDismissed);
                self.close_window(window);
            }
            FromRender::TextInput { window, commit, preedit } => {
                let Some(window) = of(self, window) else { return };
                for ime in Ime::from_protocol(commit, preedit) {
                    self.tell(handler, window, WindowEvent::Ime(ime));
                }
            }
            // Nothing is promised to other programs: all the clipboard's
            // data goes with its offer.
            FromRender::ProvideSelection { token, .. } => {
                send(&self.backend.tx, ToRender::SelectionData { token, data: None });
            }
            FromRender::DndEnter { drag, window, x, y, mimes, actions, urls } => {
                let listed = urls.as_ref().map(|(mime, data)| clipboard::parse_urls(mime, data)).unwrap_or_default();
                clipboard::shared_for(Source::Drag).drag_offered(drag, mimes.clone(), urls);
                let Some(window) = of(self, window) else {
                    self.answer_drag(drag, None, false);
                    return;
                };
                let d =
                    Drag { position: Point::new(x, y), mimes, urls: listed, copy_allowed: false, move_allowed: false };
                self.drags.insert(drag, DragState { window, drag: d, response: DropResponse::REFUSE });
                self.drag_allowed(drag, actions);
                self.drag_update(handler, drag);
            }
            FromRender::DndMotion { drag, x, y } => {
                if let Some(d) = self.drags.get_mut(&drag) {
                    d.drag.position = Point::new(x, y);
                }
                self.drag_update(handler, drag);
            }
            FromRender::DndActions { drag, actions } => {
                self.drag_allowed(drag, actions);
                self.drag_update(handler, drag);
            }
            FromRender::DndTick { drag } => self.drag_update(handler, drag),
            FromRender::DndLeave { drag } => {
                if let Some(d) = self.drags.remove(&drag) {
                    handler.drag_left(self, d.window);
                }
            }
            FromRender::DndDrop { drag } => {
                let performed = match self.drags.remove(&drag) {
                    Some(d) if d.response.action != DropAction::None && self.windows.contains_key(&d.window) => {
                        handler.dropped(self, d.window, &d.drag, &d.response)
                    }
                    Some(d) => {
                        handler.drag_left(self, d.window);
                        false
                    }
                    None => false,
                };
                send(&self.backend.tx, ToRender::DndFinish { drag, performed });
            }
            FromRender::ScreensChanged => handler.outputs_changed(self),
            FromRender::WindowOutputs { window, outputs } => {
                let Some(window) = of(self, window) else { return };
                if let Some(w) = self.windows.get_mut(&window) {
                    w.outputs = outputs;
                }
            }
            // Layer trees and display links aren't part of this toolkit
            // yet; a repaint is a repaint.
            FromRender::Tick { .. } => {}
            FromRender::Repaint { window, rects, .. } => {
                let Some(window) = of(self, window) else { return };
                if let Some(w) = self.windows.get_mut(&window) {
                    for r in rects {
                        w.invalidate(r);
                    }
                }
            }
        }
    }

    /// The source of drag `drag` now allows Wayland `actions`.
    fn drag_allowed(&mut self, drag: u64, actions: u32) {
        if let Some(d) = self.drags.get_mut(&drag) {
            d.drag.copy_allowed = actions & DND_COPY != 0;
            d.drag.move_allowed = actions & (DND_MOVE | DND_ASK) != 0;
        }
    }

    /// Ask the handler about drag `drag` where it is, and answer the render
    /// thread (which waits for an answer to each position).
    fn drag_update(&mut self, handler: &mut impl Handler, drag: u64) {
        let Some(d) = self.drags.get(&drag) else {
            self.answer_drag(drag, None, false);
            return;
        };
        let (window, info) = (d.window, d.drag.clone());
        let mut response = handler.drag_moved(self, window, &info);
        response.action = match response.action {
            DropAction::Move if info.move_allowed => DropAction::Move,
            DropAction::Move | DropAction::Copy if info.copy_allowed => DropAction::Copy,
            DropAction::Copy if info.move_allowed => DropAction::Move,
            _ => DropAction::None,
        };
        // The type asked for if the drag offers it, else the toolkit's.
        let offered = |m: &String| info.mimes.contains(m);
        response.mime = (response.action != DropAction::None)
            .then(|| {
                response.mime.take().filter(offered).or_else(|| {
                    clipboard::url_mime(&info.mimes)
                        .filter(|_| !info.urls.is_empty())
                        .map(str::to_owned)
                        .or_else(|| clipboard::text_mime(&info.mimes))
                        .or_else(|| info.mimes.first().cloned())
                })
            })
            .flatten();
        if response.mime.is_none() {
            response.action = DropAction::None;
        }
        let Some(d) = self.drags.get_mut(&drag) else {
            self.answer_drag(drag, None, false);
            return;
        };
        d.response = response.clone();
        let accept = response.mime.map(|m| (m, response.action));
        self.answer_drag(drag, accept, response.periodic);
    }

    fn answer_drag(&mut self, drag: u64, accept: Option<(String, DropAction)>, periodic: bool) {
        let (mime, actions) = match accept {
            Some((mime, DropAction::Copy)) => (Some(mime), DND_COPY),
            Some((mime, DropAction::Move)) => (Some(mime), DND_MOVE),
            _ => (None, 0),
        };
        send(&self.backend.tx, ToRender::DndStatus { drag, mime, actions, preferred: actions, periodic });
    }

    /// Each window that needs it draws, if the render thread is ready for
    /// its next frame.
    fn display(&mut self, handler: &mut impl Handler) {
        // A first frame waits (briefly) for the desktop's light or dark.
        if !settings::ready() {
            return;
        }
        let ready: Vec<WindowId> = self.windows.iter().filter(|(_, w)| w.wants_pass()).map(|(id, _)| *id).collect();
        let appearance = Appearance::system();
        for id in ready {
            let Some(w) = self.windows.get_mut(&id) else { continue };
            if !w.wants_pass() {
                continue;
            }
            let damage = std::mem::take(&mut w.damage);
            let background = match w.background {
                Background::System => Some(appearance.color(SystemColor::WindowBackground)),
                Background::Color(c) => Some(c),
                Background::Transparent => None,
            };
            let mut canvas = Canvas::new(w.size, w.scale, damage.clone(), background, appearance);
            if !damage.is_empty() {
                handler.draw(self, id, &mut canvas);
            }
            // The handler may have closed or hidden it.
            let Some(w) = self.windows.get_mut(&id) else { continue };
            let Some(showing) = w.showing else { continue };
            if std::mem::take(&mut w.title_dirty) {
                let text = if w.titlebar > 0 { title_text(&w.title) } else { TitleText::default() };
                send(&self.backend.tx, ToRender::SetTitle { window: showing, title: w.title.clone(), text });
            }
            if !damage.is_empty() {
                let (ops, temporaries) = canvas.finish();
                send(&self.backend.tx, ToRender::Paint { window: showing, target: Target::Root, rects: damage, ops });
                // Images made for this pass go once the render thread drew
                // them: what drops them comes after the paint.
                drop(temporaries);
            }
            send(&self.backend.tx, ToRender::Present { window: showing });
            w.frame_pending = true;
        }
    }
}

// Wayland's drag and drop actions.
const DND_COPY: u32 = 1;
const DND_MOVE: u32 = 2;
const DND_ASK: u32 = 4;

/// A coasting scroll event.
fn coast_event(c: &Coasting, delta: (f64, f64), momentum: ScrollPhase) -> WindowEvent {
    WindowEvent::Scroll {
        position: c.position,
        delta: ScrollDelta::Points(Vec2::new(delta.0, delta.1)),
        phase: ScrollPhase::None,
        momentum,
        velocity: Vec2::ZERO,
        inverted: c.inverted,
        modifiers: c.modifiers,
    }
}

/// The title in the title bar's font, in white, from the top left, for
/// the title bar the render thread draws.
fn title_text(title: &str) -> TitleText {
    if title.is_empty() {
        return TitleText::default();
    }
    let style = TextStyle::new(Font::system(backend::TITLE_SIZE).bold(), Color::WHITE);
    let size = crate::text::measure(title, &style);
    let (width, height) = (size.width.ceil() as f32, size.height.ceil() as f32);
    let mut canvas =
        Canvas::new(Size::new(f64::from(width), f64::from(height)), 1.0, Vec::new(), None, Appearance::default());
    canvas.draw_label(title, &style, Point::ZERO);
    // Labels upright at scale 1 make no images.
    let (ops, _) = canvas.finish();
    TitleText { ops, width, height }
}

/// An output (a screen), as the compositor describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    /// Stays the same while the output is there.
    pub id: u32,
    pub name: String,
    /// Where it is in the compositor's space, in points.
    pub frame: Rect,
    /// Device pixels per point.
    pub scale: f64,
    pub refresh_hz: f64,
    /// The largest window size there that leaves the desktop's panels
    /// uncovered, once the compositor has told a window.
    pub work_area: Option<Size>,
}

impl Output {
    fn of(o: &outputs::Output) -> Output {
        let (x, y, w, h) = o.rect;
        Output {
            id: o.id,
            name: o.name.clone(),
            frame: Rect::new(f64::from(x), f64::from(y), f64::from(x + w), f64::from(y + h)),
            scale: o.scale,
            refresh_hz: f64::from(o.refresh_mhz) / 1000.0,
            work_area: o.work_area.map(|(w, h)| Size::new(f64::from(w), f64::from(h))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sidestep_engine::protocol::Op;

    #[test]
    fn titles_are_white_glyphs_from_the_top_left() {
        let text = title_text("Title");
        assert!(text.width > 0.0 && text.height > 0.0);
        assert!(text.ops.iter().any(|op| matches!(op, Op::Glyphs(run) if run.color == [1.0; 4])));
        assert!(title_text("").ops.is_empty());
    }

    #[test]
    fn sleeping_wakes_for_a_signal_or_a_deadline() {
        let wake = Arc::new(Wake::default());
        let start = Instant::now();
        wake.wait(Some(start + Duration::from_millis(20)));
        assert!(start.elapsed() >= Duration::from_millis(20));
        let other = wake.clone();
        let t = std::thread::spawn(move || other.wake());
        wake.wait(None);
        t.join().unwrap();
        // A wake before the sleep isn't lost.
        wake.wake();
        let start = Instant::now();
        wake.wait(Some(start + Duration::from_secs(5)));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn drop_responses_say_what_and_how() {
        assert_eq!(DropResponse::default(), DropResponse::REFUSE);
        let r = DropResponse::copy().with_mime("text/plain").periodic();
        assert_eq!((r.action, r.mime.as_deref(), r.periodic), (DropAction::Copy, Some("text/plain"), true));
        assert_eq!(DropResponse::from(DropAction::Move), DropResponse::moving());
    }
}
