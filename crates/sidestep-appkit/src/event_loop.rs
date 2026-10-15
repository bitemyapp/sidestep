//! AppKit on the main thread's run loop.
//!
//! Foundation owns the loop (`sidestep_foundation::runloop`); AppKit plugs
//! three things into it when it starts (see [`install`]):
//!
//! - **Its modes.** `NSModalPanelRunLoopMode` and `NSEventTrackingRunLoopMode`
//!   join the common modes, so whatever is registered there (the items
//!   below, and programs' common-mode timers) also works in modal and
//!   tracking loops.
//! - **The event source**, in the common modes (and in any other mode an
//!   AppKit loop is asked to run in, see [`serve`]). The render thread
//!   signals it after every message it sends ([`MainSender`]). Performing
//!   it moves the messages into an inbox and handles them oldest first:
//!   configures, frames, focus and close requests act at once, and input
//!   becomes `NSEvent`s in the application's queue. The inbox is shared by
//!   every drain, so a loop nested inside an event handler carries on with
//!   the messages after the one being handled, in order. Moves that another
//!   move of the same window follows are coalesced, as AppKit coalesces
//!   mouse events, and key repeats followed by a newer key message for the
//!   same window are dropped, so a busy main thread catches up rather than
//!   falling behind.
//! - **The display pass**, an observer of every run in the common modes
//!   before it sleeps and when it ends, so modal, tracking and
//!   terminate-later loops keep drawing. Each window displays if the render
//!   thread has shown its last frame; tracking areas look again at views
//!   that moved; pasteboard changes are offered.
//!
//! Only AppKit's own loops take events from the queue, as on macOS:
//! `-[NSApplication run]` runs the loop in the default mode and sends each
//! event through `-[NSApplication sendEvent:]`, a modal loop does the same
//! in `NSModalPanelRunLoopMode`, and `nextEventMatchingMask:untilDate:
//! inMode:dequeue:` runs the loop in the mode it is given (timers and
//! sources of that mode only) and hands its caller the event it asked for.
//! A program that runs the run loop itself (`-[NSRunLoop runMode:
//! beforeDate:]`, waiting for something inside an event handler, say)
//! leaves events queued, so handlers are never entered from inside one
//! another. Each AppKit loop returns to its caller after every batch of
//! input, sends the events waiting, and stops once its condition holds;
//! whatever ends it (`stop:`, `stopModal`) also stops the innermost run, so
//! a timer's callout ends it without waiting for input. (The callouts of
//! tracking areas are the exception: they still come as the pointer moves,
//! from whichever run handles the move.)
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
use std::sync::mpsc::TryRecvError;
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
pub(crate) const DISPLAY_ORDER: isize = 2_000_000;

thread_local! {
    /// The event source's signal, once [`install`] has made it.
    static SIGNAL: RefCell<Option<SourceSignal>> = const { RefCell::new(None) };
    /// Modes outside the common modes the event source and the display
    /// pass were added to, for loops looking for events there.
    static SERVED: RefCell<Vec<Mode>> = const { RefCell::new(Vec::new()) };
    /// The event source is performing: what it queues needs no signal.
    static PERFORMING: Cell<bool> = const { Cell::new(false) };
    /// Messages from the render thread not handled yet, oldest first.
    static INBOX: RefCell<VecDeque<FromRender>> = const { RefCell::new(VecDeque::new()) };
    /// The depth of the run an AppKit loop is in (0: none): the run that
    /// looks at the queue when it returns.
    static APPKIT_RUN: Cell<usize> = const { Cell::new(0) };
    /// Events were queued that the AppKit loop running may not have looked
    /// at: queued inside a run nested in one of its callouts (a timer that
    /// runs the loop itself), which the AppKit loop's run doesn't return
    /// for. Its run returns to look before it sleeps.
    static UNSEEN: Cell<bool> = const { Cell::new(false) };
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
    add_display_pass(&[Mode::COMMON]);
    SIGNAL.with(|s| s.replace(Some(signal.clone())));
    signal
}

fn add_display_pass(modes: &[Mode]) {
    let activities = Activity::BEFORE_WAITING | Activity::EXIT;
    runloop::main().add_observer(modes, activities, DISPLAY_ORDER, end_of_turn);
}

/// Have AppKit's event source and display pass work in `mode` too, for a
/// loop looking for events there: the common modes have them already, and
/// any other mode gets them the first time (as a CoreFoundation source can
/// be in several modes), so its loop gets input and draws. Other
/// common-mode items (a program's timers) don't join it.
fn serve(mode: Mode) {
    if mode == Mode::DEFAULT || mode == tracking_mode() || mode == modal_mode() {
        return;
    }
    if SERVED.with(|s| s.borrow().contains(&mode)) {
        return;
    }
    SERVED.with(|s| s.borrow_mut().push(mode));
    let rl = runloop::main();
    if rl.is_common_mode(mode) {
        return;
    }
    if let Some(signal) = SIGNAL.with(|s| s.borrow().clone()) {
        rl.add_source_mode(&signal, mode);
    }
    add_display_pass(&[mode]);
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

/// Something was queued: the loop running now returns to its caller (an
/// AppKit loop, which sends or takes it) by way of the event source, unless
/// the source is performing already.
pub(crate) fn queued() {
    UNSEEN.with(|u| u.set(true));
    if !PERFORMING.with(Cell::get) {
        signal();
    }
}

/// The event source's callout. Input only joins the queue: the AppKit loop
/// running (if any) sends or takes it once the run returns.
fn perform() {
    let outer = PERFORMING.with(|p| p.replace(true));
    take_from_render();
    while let Some(msg) = next_message() {
        handle(msg);
    }
    app::settle_focus();
    app::update_active();
    PERFORMING.with(|p| p.set(outer));
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
                // A stalled loop hears only the latest of a window's frames.
                FromRender::Tick { window, .. } => {
                    inbox.iter().any(|m| matches!(m, FromRender::Tick { window: w, .. } if w == window))
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
        // Pasteboards and drag and drop (pasteboard.rs, drag.rs).
        FromRender::ProvideSelection { mime, token } => crate::pasteboard::provide_for_render(&mime, token),
        FromRender::DndEnter { drag, window, x, y, mimes, actions, urls } => {
            crate::drag::enter(drag, app::find_window(window).as_deref(), (x, y), mimes, actions, urls);
        }
        FromRender::DndMotion { drag, x, y } => crate::drag::motion(drag, x, y),
        FromRender::DndActions { drag, actions } => crate::drag::actions(drag, actions),
        FromRender::DndTick { drag } => crate::drag::tick(drag),
        FromRender::DndLeave { drag } => crate::drag::leave(drag),
        FromRender::DndDrop { drag } => crate::drag::dropped(drag),
        // Screens (screen.rs).
        FromRender::ScreensChanged => {
            crate::screen::changed(objc2::MainThreadMarker::new().expect("the main thread"));
        }
        FromRender::WindowOutputs { window, outputs } => {
            if let Some(w) = app::find_window(window) {
                crate::screen::window_outputs(&w, outputs);
            }
        }
        // Core Animation (quartzcore).
        FromRender::Tick { window, time } => crate::quartzcore::display_link::tick(window, time),
        FromRender::Repaint { window, target, rects } => {
            if let Some(w) = app::find_window(window) {
                let key = match target {
                    crate::protocol::Target::Root => crate::protocol::ROOT_LAYER,
                    crate::protocol::Target::Tiles(l) => l,
                    crate::protocol::Target::Overlay(l) => crate::layers::damage_key(l, true),
                    crate::protocol::Target::Content(_) => return,
                };
                for r in rects {
                    window::imp(&w).invalidate(key, r);
                }
            }
        }
    }
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
    serve(mode);
    // The first look for events after `finishLaunching` finishes launching.
    app::launched();
    struct Leaving;
    impl Drop for Leaving {
        fn drop(&mut self) {
            // Events this loop didn't take are for the AppKit loop outside
            // it, which may be in the middle of a run (this loop ran in one
            // of its timers): it looks before it sleeps.
            if app::has_queued() {
                UNSEEN.with(|u| u.set(true));
            }
        }
    }
    let _leaving = Leaving;
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
        let result = appkit_run(mode, limit);
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
    serve(mode);
    while running() {
        autoreleasepool(|_| {
            app::send_queued();
            if running() {
                appkit_run(mode, None);
            }
        });
    }
}

/// Run the loop in `mode` for one of AppKit's loops, returning after a
/// batch of input (or `limit`): the loop then looks at the queue.
fn appkit_run(mode: Mode, limit: Option<Instant>) -> RunResult {
    let rl = runloop::main();
    struct Outer(usize);
    impl Drop for Outer {
        fn drop(&mut self) {
            APPKIT_RUN.with(|d| d.set(self.0));
        }
    }
    let _outer = Outer(APPKIT_RUN.with(|d| d.replace(rl.depth() + 1)));
    resume_inbox();
    let result = rl.run_mode(mode, limit, true);
    UNSEEN.with(|u| u.set(false));
    result
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
    run_until(mode, Instant::now());
}

/// Run the loop in `mode` until the first batch of input or `limit`,
/// sending the events waiting before and after.
pub(crate) fn run_until(mode: Mode, limit: Instant) {
    install();
    serve(mode);
    autoreleasepool(|_| {
        app::send_queued();
        appkit_run(mode, Some(limit));
        app::send_queued();
    });
}

/// End the innermost run of the main loop, so the loop that ran it looks at
/// its condition again even if nothing comes.
pub(crate) fn stop_innermost() {
    runloop::main().stop();
}

/// Before the loop sleeps, and when a run ends: display, offer the
/// pasteboard, and arm AppKit's deadline timer; and before an AppKit loop's
/// run sleeps with events it hasn't looked at, have it return instead.
fn end_of_turn(activity: Activity) {
    crate::pasteboard::offer_changes();
    app::refresh_windows();
    // After the paints: bitmaps that went away, the render thread forgets.
    crate::image_rep::send_forgotten();
    // A first frame held back for the desktop's appearance shows by then.
    let due = earliest(earliest(crate::momentum::deadline(), app::resign_deadline()), crate::settings::deadline());
    if let Some(due) = due {
        arm(due);
    }
    if activity == Activity::BEFORE_WAITING
        && UNSEEN.with(Cell::get)
        && APPKIT_RUN.with(Cell::get) == runloop::main().depth()
        && app::has_queued()
    {
        UNSEEN.with(|u| u.set(false));
        if let Some(signal) = SIGNAL.with(|s| s.borrow().clone()) {
            signal.signal_and_wake();
        }
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
/// they don't pile up while nobody takes them, as on macOS. Starting them
/// while they are on is a mistake macOS raises for (Sidestep panics).
pub(crate) fn start_periodic(delay: f64, period: f64) {
    if PERIODIC.with(|p| p.borrow().is_some()) {
        panic!("*** +[NSEvent startPeriodicEventsAfterDelay:withPeriod:]: periodic events are already on");
    }
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
