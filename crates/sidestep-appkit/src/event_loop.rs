//! AppKit on the main thread's run loop.
//!
//! Foundation owns the loop (`sidestep_foundation::runloop`); AppKit plugs
//! three things into it when it starts (see [`install`]):
//!
//! - **Its modes.** `NSModalPanelRunLoopMode` and `NSEventTrackingRunLoopMode`
//!   join the common modes, so whatever is registered there (the items
//!   below, and programs' common-mode timers) also works in modal and
//!   tracking loops.
//! - **The event source**, in the common modes. The render thread signals
//!   it after every message it sends ([`MainSender`]). Performing it moves
//!   the messages into an inbox and handles them oldest first: configures,
//!   frames, focus and close requests act at once, and input becomes
//!   `NSEvent`s sent through `-[NSApplication sendEvent:]` — or queued,
//!   while a nested loop looks for events of its own. The inbox is shared
//!   by every drain, so a loop nested inside an event handler carries on
//!   with the messages after the one being handled, in order. Moves that
//!   another move of the same window follows are coalesced, as AppKit
//!   coalesces mouse events, and key repeats followed by a newer key
//!   message for the same window are dropped, so a busy main thread
//!   catches up rather than falling behind.
//! - **The display pass**, an observer of every run in the common modes
//!   before it sleeps and when it ends, so modal, tracking and
//!   terminate-later loops keep drawing. Each window displays if the render
//!   thread has shown its last frame; tracking areas look again at views
//!   that moved; pasteboard changes are offered.
//!
//! `-[NSApplication run]` runs the loop in the default mode, a modal loop in
//! `NSModalPanelRunLoopMode`, and `nextEventMatchingMask:untilDate:inMode:
//! dequeue:` in the mode it is given: timers and sources of that mode only.
//! Each returns to its caller after every batch of input, sends the events
//! nested loops left, and stops once its condition holds; whatever ends it
//! (`stop:`, `stopModal`) also stops the innermost run, so a timer's callout
//! ends it without waiting for input.
//!
//! AppKit's own deadlines (touchpad momentum without frames, the pause
//! before deciding the application lost focus) share one timer in the
//! common modes, set before the loop sleeps when one comes due sooner than
//! it would fire. An idle loop sleeps with nothing armed and allocates
//! nothing per turn.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{NSEvent, NSEventMask};
use objc2_foundation::{NSDate, NSRunLoop, NSRunLoopCommonModes, NSTimer};
use sidestep_foundation::runloop::{self, Activity, Mode, RunResult, SourceSignal};

use crate::app;
use crate::protocol::FromRender;
use crate::window;

// The run-loop modes AppKit adds, as macOS names them.
sidestep_foundation::constant_string!(NSEventTrackingRunLoopMode = "NSEventTrackingRunLoopMode");
sidestep_foundation::constant_string!(NSModalPanelRunLoopMode = "NSModalPanelRunLoopMode");

/// `NSEventTrackingRunLoopMode`, which controls tracking the mouse run in.
pub(crate) fn tracking_mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| Mode::named("NSEventTrackingRunLoopMode"))
}

/// `NSModalPanelRunLoopMode`, which modal loops run in.
pub(crate) fn modal_mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| Mode::named("NSModalPanelRunLoopMode"))
}

/// Where the display pass comes among a run's observers: late, after the
/// program's own before-waiting observers (Core Animation's commit has the
/// same place on macOS).
const DISPLAY_ORDER: isize = 2_000_000;

thread_local! {
    /// The event source's signal, once [`install`] has made it.
    static SIGNAL: RefCell<Option<SourceSignal>> = const { RefCell::new(None) };
    /// Messages from the render thread not handled yet, oldest first.
    static INBOX: RefCell<VecDeque<FromRender>> = const { RefCell::new(VecDeque::new()) };
    /// Nested loops looking for events; while there are any, input is
    /// queued rather than sent.
    static PUMPING: Cell<u32> = const { Cell::new(0) };
    /// AppKit's deadline timer, and when it is set to fire.
    static WAKE: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    static WAKE_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The timer posting periodic events, while they are on.
    static PERIODIC: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
}

/// Plug AppKit into the main thread's run loop, once. Called when
/// `NSApplication` is made and before the render thread starts, so the
/// render thread's messages always have a source to wake.
pub(crate) fn install() -> SourceSignal {
    if let Some(signal) = SIGNAL.with(|s| s.borrow().clone()) {
        return signal;
    }
    let rl = runloop::main();
    assert!(rl.is_current(), "sidestep: AppKit belongs to the main thread");
    rl.add_common_mode(modal_mode());
    rl.add_common_mode(tracking_mode());
    let signal = rl.add_source(&[Mode::COMMON], 0, perform);
    rl.add_observer(&[Mode::COMMON], Activity::BEFORE_WAITING | Activity::EXIT, DISPLAY_ORDER, |_| end_of_turn());
    SIGNAL.with(|s| s.replace(Some(signal.clone())));
    signal
}

/// Have the event source look at the inbox and the queue on the loop's next
/// pass.
pub(crate) fn signal() {
    SIGNAL.with(|s| {
        if let Some(signal) = &*s.borrow() {
            signal.signal();
        }
    });
}

/// The render thread's sender to the main thread: each message wakes the
/// main loop through the event source. When the render thread stops (a
/// Wayland protocol error ends its connection) the sender goes away, which
/// also wakes the loop, to see the channel closed.
pub(crate) struct MainSender {
    tx: Option<mpsc::Sender<FromRender>>,
    signal: SourceSignal,
}

impl MainSender {
    pub(crate) fn new(tx: mpsc::Sender<FromRender>, signal: SourceSignal) -> MainSender {
        MainSender { tx: Some(tx), signal }
    }

    pub(crate) fn send(&self, msg: FromRender) -> Result<(), mpsc::SendError<FromRender>> {
        self.tx.as_ref().expect("the sender is only taken when dropped").send(msg)?;
        self.signal.signal_and_wake();
        Ok(())
    }
}

impl Drop for MainSender {
    fn drop(&mut self) {
        // Closed first, so the woken loop finds it closed.
        drop(self.tx.take());
        self.signal.signal_and_wake();
    }
}

/// The event source's callout.
fn perform() {
    take_from_render();
    while let Some(msg) = next_message() {
        handle(msg);
    }
    app::settle_focus();
    app::update_active();
    if PUMPING.with(Cell::get) == 0 {
        app::send_queued();
    }
}

/// Move what the render thread sent into the inbox. If the render thread
/// stopped, so does the program: it has no windows any more.
pub(crate) fn take_from_render() {
    let closed = app::with_receiver(|rx| {
        INBOX.with(|inbox| {
            let mut inbox = inbox.borrow_mut();
            loop {
                match rx.try_recv() {
                    Ok(msg) => inbox.push_back(msg),
                    Err(TryRecvError::Empty) => return false,
                    Err(TryRecvError::Disconnected) => return true,
                }
            }
        })
    });
    if closed == Some(true) {
        eprintln!("sidestep: the render thread stopped");
        std::process::exit(1);
    }
}

/// Add a message to the inbox as if the render thread had sent it (for
/// tests, through `testing`), after what it did send.
pub(crate) fn inject(msg: FromRender) {
    take_from_render();
    INBOX.with(|inbox| inbox.borrow_mut().push_back(msg));
    signal();
}

/// Whether messages from the render thread wait to be handled.
pub(crate) fn has_inbox() -> bool {
    INBOX.with(|inbox| !inbox.borrow().is_empty())
}

/// The next message worth handling. A move followed by another move of the
/// same window is dropped, and so is a key repeat followed by any newer key
/// message for its window.
fn next_message() -> Option<FromRender> {
    INBOX.with(|inbox| {
        let mut inbox = inbox.borrow_mut();
        loop {
            let msg = inbox.pop_front()?;
            let superseded = match &msg {
                FromRender::Motion { window, .. } => {
                    matches!(inbox.front(), Some(FromRender::Motion { window: next, .. }) if next == window)
                }
                FromRender::Key { window, key } if key.repeat => {
                    inbox.iter().any(|m| matches!(m, FromRender::Key { window: w, .. } if w == window))
                }
                _ => false,
            };
            if !superseded {
                return Some(msg);
            }
        }
    })
}

/// Act on a message from the render thread.
fn handle(msg: FromRender) {
    match msg {
        FromRender::Configure { window, width, height, scale, titlebar, state } => {
            if let Some(w) = app::find_window(window) {
                window::imp(&w).configure(width, height, scale, titlebar, state);
            }
        }
        FromRender::Frame { window } => {
            if let Some(w) = app::find_window(window) {
                window::imp(&w).frame_done();
                crate::momentum::frame(&w);
            }
        }
        FromRender::Focus { window, focused } => app::keyboard_focus(window, focused),
        FromRender::Key { window, key } => {
            let Some(w) = app::keys_window(window) else { return };
            let event = crate::event::key_event(&w, key);
            app::dispatch(&event);
            // Typing moves the caret an input method places its window by.
            crate::inputcontext::update(window::imp(&w));
        }
        FromRender::Modifiers { window, modifiers, code } => {
            crate::event::set_current_flags(modifiers);
            let Some(w) = app::keys_window(window) else { return };
            let event = crate::event::flags_changed_event(&w, modifiers, code);
            app::dispatch(&event);
        }
        FromRender::Enter { window, x, y } => {
            if let Some(w) = app::find_window(window) {
                window::imp(&w).motion(x, y, crate::event::current_flags());
            }
        }
        FromRender::Leave { window } => {
            if let Some(w) = app::find_window(window) {
                window::imp(&w).pointer_left();
            }
        }
        FromRender::Button { window, x, y, button, pressed, clicks, modifiers, activating } => {
            if let Some(w) = app::find_window(window) {
                window::imp(&w).button(button, pressed, x, y, clicks, modifiers, activating);
            }
        }
        FromRender::Motion { window, x, y, modifiers } => {
            if let Some(w) = app::find_window(window) {
                window::imp(&w).motion(x, y, modifiers);
            }
        }
        FromRender::Scroll { window, x, y, dx, dy, wheel, modifiers, phase, velocity, inverted } => {
            if let Some(w) = app::find_window(window) {
                window::imp(&w).scroll((x, y), (dx, dy), wheel, modifiers, (phase, velocity, inverted));
            }
        }
        FromRender::Pinch { window, x, y, phase, magnification, rotation, modifiers } => {
            if let Some(w) = app::find_window(window) {
                window::imp(&w).pinch((x, y), phase, magnification, rotation, modifiers);
            }
        }
        FromRender::CloseRequested { window } => {
            if let Some(w) = app::find_window(window) {
                w.performClose(None);
            }
        }
        FromRender::PopupDone { window } => {
            if let Some(w) = app::find_window(window) {
                w.orderOut(None);
            }
        }
        FromRender::TextInput { window, commit, preedit } => {
            if let Some(w) = app::find_window(window) {
                crate::inputcontext::apply(window::imp(&w), commit, preedit);
            }
        }
    }
}

/// Whether a nested loop is looking for events, so input waits for it.
pub(crate) fn pumping() -> bool {
    PUMPING.with(Cell::get) > 0
}

/// Take (or look at) the first queued event `mask` matches, running the loop
/// in `mode` until one comes or `deadline` passes (`None`: never). Events it
/// doesn't want wait for the loop outside.
pub(crate) fn next_event(
    mask: NSEventMask,
    deadline: Option<Instant>,
    dequeue: bool,
    mode: Mode,
) -> Option<Retained<NSEvent>> {
    install();
    // The first look for events after `finishLaunching` finishes launching.
    app::launched();
    let rl = runloop::main();
    struct Pumping;
    impl Drop for Pumping {
        fn drop(&mut self) {
            let left = PUMPING.with(|p| {
                p.set(p.get() - 1);
                p.get()
            });
            // Events this loop didn't take go to the loop outside it.
            if left == 0 && app::has_queued() {
                signal();
            }
        }
    }
    PUMPING.with(|p| p.set(p.get() + 1));
    let _pumping = Pumping;
    let mut looked = false;
    loop {
        if let Some(event) = app::take_queued(mask, dequeue) {
            return Some(event);
        }
        let now = Instant::now();
        let expired = deadline.is_some_and(|d| d <= now);
        if expired && looked {
            return None;
        }
        let limit = if expired { Some(now) } else { deadline };
        resume_inbox();
        let result = rl.run_mode(mode, limit, true);
        looked = true;
        if result == RunResult::Finished {
            // Nothing in the mode could bring an event.
            return app::take_queued(mask, dequeue);
        }
    }
}

/// Run the loop in `mode`, sending events as they come, while `running`
/// holds: the main loop, modal loops and the wait for a reply to
/// `applicationShouldTerminate:`. Whatever makes `running` false should
/// also stop the innermost run (see [`stop_innermost`]).
pub(crate) fn run_while(mode: Mode, running: &dyn Fn() -> bool) {
    install();
    let rl = runloop::main();
    while running() {
        autoreleasepool(|_| {
            app::send_queued();
            if running() {
                resume_inbox();
                rl.run_mode(mode, None, true);
            }
        });
    }
}

/// A loop nested in an event handler carries on with the messages that
/// came with the event: the source, already performing, is signalled again
/// for them.
fn resume_inbox() {
    if has_inbox() {
        signal();
    }
}

/// One pass of the loop in `mode`, without waiting, sending the events that
/// come (a modal session's `runModalSession:`).
pub(crate) fn run_once(mode: Mode) {
    install();
    autoreleasepool(|_| {
        app::send_queued();
        resume_inbox();
        runloop::main().run_mode(mode, Some(Instant::now()), true);
        app::send_queued();
    });
}

/// End the innermost run of the main loop, so the loop that ran it looks at
/// its condition again even if nothing comes.
pub(crate) fn stop_innermost() {
    runloop::main().stop();
}

/// Before the loop sleeps, and when a run ends: display, offer the
/// pasteboard, and arm AppKit's deadline timer.
fn end_of_turn() {
    crate::pasteboard::offer_changes();
    app::refresh_windows();
    let due = earliest(crate::momentum::deadline(), app::resign_deadline());
    if let Some(due) = due {
        arm(due);
    }
}

fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Have the deadline timer fire by `due`. A timer set for later is brought
/// forward; one set for sooner fires early and is set again then.
fn arm(due: Instant) {
    if WAKE_AT.with(Cell::get).is_some_and(|at| at <= due) {
        return;
    }
    WAKE_AT.with(|w| w.set(Some(due)));
    let date = NSDate::dateWithTimeIntervalSinceNow(due.saturating_duration_since(Instant::now()).as_secs_f64());
    let existing = WAKE.with(|w| w.borrow().clone());
    match existing {
        Some(timer) => timer.setFireDate(&date),
        None => {
            // Repeating, so it stays valid between deadlines; each fire sets
            // its next date past any deadline, and `arm` brings it back.
            let block = RcBlock::new(|_: NonNull<NSTimer>| deadline_passed());
            // SAFETY: the block runs on the main thread, where the timer is
            // scheduled, and captures nothing.
            let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(1e9, true, &block) };
            timer.setFireDate(&date);
            // SAFETY: the main loop takes the timer; NSRunLoopCommonModes is
            // Foundation's constant.
            unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
            WAKE.with(|w| w.replace(Some(timer)));
        }
    }
}

/// The deadline timer fired: step what is due.
fn deadline_passed() {
    WAKE_AT.with(|w| w.set(None));
    let now = Instant::now();
    crate::momentum::tick(now);
    app::resign_if_due();
}

/// `+[NSEvent startPeriodicEventsAfterDelay:withPeriod:]`: a periodic event
/// joins the queue after `delay` seconds and every `period` after, in any
/// common mode (tracking loops take them), but only when none is waiting:
/// they don't pile up while nobody takes them, as on macOS.
pub(crate) fn start_periodic(delay: f64, period: f64) {
    stop_periodic();
    install();
    let block = RcBlock::new(|_: NonNull<NSTimer>| {
        if !app::has_queued_matching(NSEventMask::Periodic) {
            app::post_event(&crate::event::periodic_event(), false);
        }
    });
    // SAFETY: the block runs on the main thread, where the timer is
    // scheduled.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(period.max(0.0001), true, &block) };
    timer.setFireDate(&NSDate::dateWithTimeIntervalSinceNow(delay.max(0.0)));
    // SAFETY: the main loop takes the timer; NSRunLoopCommonModes is
    // Foundation's constant.
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    PERIODIC.with(|p| p.replace(Some(timer)));
}

/// `+[NSEvent stopPeriodicEvents]`: no more, and none left waiting.
pub(crate) fn stop_periodic() {
    if let Some(timer) = PERIODIC.with(|p| p.take()) {
        timer.invalidate();
    }
    app::discard_events(NSEventMask::Periodic, None);
}
