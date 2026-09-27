//! `CATransaction`, and the commit that hands layer changes over.
//!
//! Each thread has a stack of explicit transactions (`begin`/`commit`)
//! and, once a layer changes outside one, an implicit transaction that the
//! run loop commits before it next waits (an observer just ahead of
//! AppKit's display pass on the main thread). A transaction's settings
//! (duration, disabled actions, timing function, any other key) are
//! looked up from the innermost transaction out, so a nested one starts
//! with its parent's, and the defaults (a quarter second, actions on)
//! apply outside any.
//!
//! **Completion blocks.** Setting a transaction's completion block starts
//! a new completion group: the animations added from then on, in it or in
//! transactions nested in it, are the ones the block waits for (so a block
//! set after its transaction's animations were added runs at once, as on
//! macOS), and a block replaced by another still runs, when its own
//! animations are done. A block runs from the run loop after its
//! transaction commits and its animations have stopped, ahead of their
//! delegates' `animationDidStop:finished:`.
//!
//! **Commits.** The outermost explicit commit, `flush`, and the end of the
//! turn commit every layer changed since the last commit: layers needing
//! layout are laid out and those needing display display (a view's layer
//! through its view's `updateLayer` when it wants that; other views draw
//! into their canvases in the display pass); layers in the tree of a
//! window that has a backing (not a deferred window never shown) become
//! live (implicit animations and presentation layers start with the first
//! commit, as on macOS), their properties are what their presentation
//! layers show from then on, and their new animations start (a begin time
//! of 0 becoming the layer's time now); animations of layers in no such
//! tree stop unfinished and go, as macOS stops them; and the layers in the
//! trees of windows on screen go to the render thread as one
//! `ToRender::Commit`, each changed layer whole (properties, sublayers,
//! animations, and its contents when they changed). The render thread
//! animates from there on its own. Delegates hear `animationDidStart:`
//! from the run loop after the commit, and animations removed since the
//! last commit report their `animationDidStop:finished:` then.
//!
//! **Endings.** The main thread doesn't follow animations frame by frame:
//! it sets one timer for the earliest end among started animations still
//! running (or the earliest begin time still to come, to wake the render
//! thread then), and then tells delegates (`animationDidStop:finished:`),
//! removes finished animations that ask to be, marks the others ended (so
//! nothing waits for them any more), and runs completion blocks whose
//! animations are all done, from the run loop.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use block2::{DynBlock, RcBlock};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, Message, define_class, msg_send, sel};
use objc2_foundation::{NSString, NSTimer};
use objc2_quartz_core::{CAAnimation, CALayer, CAMediaTimingFunction};

use super::layer::{self, Added, CALayerImpl, LayerId, imp};
use super::math;
use sidestep_foundation::runloop::{self, Activity, Mode};

/// The default duration of animations (`kCATransactionAnimationDuration`).
pub(crate) const DEFAULT_DURATION: f64 = 0.25;

sidestep_foundation::constant_string!(kCATransactionAnimationDuration = "animationDuration");
sidestep_foundation::constant_string!(kCATransactionDisableActions = "disableActions");
sidestep_foundation::constant_string!(kCATransactionAnimationTimingFunction = "animationTimingFunction");
sidestep_foundation::constant_string!(kCATransactionCompletionBlock = "completionBlock");

/// A completion block and the animations it waits for.
pub(crate) struct Group {
    pending: Cell<usize>,
    committed: Cell<bool>,
    block: RefCell<Option<RcBlock<dyn Fn()>>>,
}

impl Group {
    fn new(block: Option<RcBlock<dyn Fn()>>) -> Rc<Group> {
        Rc::new(Group { pending: Cell::new(0), committed: Cell::new(false), block: RefCell::new(block) })
    }

    /// An animation it waited for stopped.
    fn done(&self) {
        self.pending.set(self.pending.get().saturating_sub(1));
        self.maybe_fire();
    }

    fn maybe_fire(&self) {
        if self.committed.get()
            && self.pending.get() == 0
            && let Some(block) = self.block.borrow_mut().take()
        {
            later(move || block.call(()));
        }
    }
}

#[derive(Default)]
struct Frame {
    duration: Option<f64>,
    disable: Option<bool>,
    function: Option<Option<Retained<CAMediaTimingFunction>>>,
    values: Vec<(String, Retained<AnyObject>)>,
    /// The completion group animations added now join (made by
    /// `setCompletionBlock:`), and the ones it replaced.
    group: Option<Rc<Group>>,
    replaced: Vec<Rc<Group>>,
}

impl Frame {
    /// The transaction ends: its completion groups are committed, and run
    /// when their animations are done.
    fn close(self) {
        for g in self.replaced.iter().chain(self.group.iter()) {
            g.committed.set(true);
            g.maybe_fire();
        }
    }
}

#[derive(Default)]
struct State {
    stack: Vec<Frame>,
    /// The implicit transaction's settings, made when one is set outside
    /// any explicit transaction; it ends with the turn.
    implicit_frame: Option<Frame>,
    /// Changes waiting for a commit (the implicit transaction).
    implicit: bool,
    dirty: Vec<Retained<CALayer>>,
    observing: bool,
    /// Actions forced off while a style is applied.
    forced: Option<bool>,
}

/// An animation that stopped before its end, reported at the next commit.
struct Stop {
    added: Added,
    finished: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
    /// Layers with started animations, to end them.
    static ANIMATING: RefCell<Vec<Weak<CALayer>>> = const { RefCell::new(Vec::new()) };
    /// The timer for the next ending (or begin), and when it fires (media
    /// time).
    static ENDING: RefCell<Option<(f64, Retained<NSTimer>)>> = const { RefCell::new(None) };
    /// Layers the render thread must forget.
    static GONE: RefCell<Vec<LayerId>> = const { RefCell::new(Vec::new()) };
    /// Animations removed since the last commit, to report at the next.
    static STOPS: RefCell<Vec<Stop>> = const { RefCell::new(Vec::new()) };
    /// A commit is running (changes made meanwhile wait for the next).
    static COMMITTING: Cell<bool> = const { Cell::new(false) };
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// Run `f` from the run loop soon (completion blocks and delegate calls
/// that must not run inside a commit or a setter).
fn later(f: impl FnOnce() + 'static) {
    struct Unsend<F>(F);
    // SAFETY: the work runs on this thread's loop (perform queues it on
    // the loop it's given, which is this thread's).
    unsafe impl<F> Send for Unsend<F> {}
    let work = Unsend(f);
    runloop::current().perform(&[Mode::COMMON], move || {
        let work = work;
        (work.0)()
    });
}

fn lookup<T>(f: impl Fn(&Frame) -> Option<T>) -> Option<T> {
    with(|s| s.stack.iter().rev().chain(s.implicit_frame.as_ref()).find_map(&f))
}

pub(crate) fn animation_duration() -> f64 {
    lookup(|f| f.duration).unwrap_or(DEFAULT_DURATION)
}

pub(crate) fn disable_actions() -> bool {
    if let Some(forced) = with(|s| s.forced) {
        return forced;
    }
    lookup(|f| f.disable).unwrap_or(false)
}

/// Force actions off regardless of the transactions (for a style's
/// values); returns what was forced before, for [`restore_forced`].
pub(crate) fn force_actions_off() -> Option<bool> {
    with(|s| s.forced.replace(true))
}

pub(crate) fn restore_forced(before: Option<bool>) {
    with(|s| s.forced = before);
}

pub(crate) fn timing_function() -> Option<Retained<CAMediaTimingFunction>> {
    lookup(|f| f.function.clone()).flatten()
}

/// The completion groups an animation added now belongs to: the current
/// group of each open transaction (and of the implicit one), each counting
/// it.
pub(crate) fn current_groups() -> Vec<Rc<Group>> {
    let groups: Vec<Rc<Group>> =
        with(|s| s.implicit_frame.iter().chain(s.stack.iter()).filter_map(|f| f.group.clone()).collect());
    for g in &groups {
        g.pending.set(g.pending.get() + 1);
    }
    groups
}

// Marking changes.

/// A layer changed: remember it for the next commit, and make sure there
/// is one.
pub(crate) fn mark_dirty(layer: &CALayerImpl) {
    if layer.is_presentation() {
        // A presentation layer changes nothing.
        return;
    }
    // Its cached presentation layer is out of date.
    layer.ivars().presentation.borrow_mut().take();
    if !layer.ivars().dirty.replace(true) {
        let l = layer::as_layer(layer).retain();
        with(|s| s.dirty.push(l));
    }
    touch();
}

/// Make sure an implicit transaction will commit what changed.
pub(crate) fn touch() {
    with(|s| s.implicit = true);
    install_observer();
}

/// Have the thread's run loop end the implicit transaction before it
/// waits (once per thread).
fn install_observer() {
    if with(|s| std::mem::replace(&mut s.observing, true)) {
        return;
    }
    let rl = runloop::current();
    // Just ahead of AppKit's display pass, on the main thread.
    let order = if rl.is_main() { crate::event_loop::DISPLAY_ORDER - 1000 } else { crate::event_loop::DISPLAY_ORDER };
    rl.add_observer(&[Mode::COMMON], Activity::BEFORE_WAITING | Activity::EXIT, order, |_| end_of_turn());
}

/// The layer's sublayers or mask changed.
pub(crate) fn structure_changed(layer: &CALayerImpl) {
    mark_dirty(layer);
}

/// `layer` joined a tree: it and what's under it go whole to the next
/// commit (the tree it joined may be a window's).
pub(crate) fn attached(layer: &CALayerImpl) {
    for l in layer::tree(layer::as_layer(layer)) {
        mark_dirty(imp(&l));
    }
}

/// The layer's own timing changed: its animations end at other times.
pub(crate) fn timing_changed(layer: &CALayerImpl) {
    if layer::animating(layer) {
        touch();
    }
}

pub(crate) fn animation_added(layer: &CALayerImpl) {
    mark_dirty(layer);
}

/// Animations left a layer before they ended: their delegates hear they
/// didn't finish (those that ended already heard), and their groups
/// count them, at the next commit (as on macOS).
pub(crate) fn animations_removed(layer: &CALayerImpl, gone: Vec<Added>) {
    if gone.is_empty() {
        return;
    }
    mark_dirty(layer);
    // The latest added first (measured).
    for added in gone.into_iter().rev() {
        if !added.ended {
            stopped_later(added, false);
        }
    }
}

/// Report an animation's stop at the next commit (and make sure there is
/// one).
pub(crate) fn stopped_later(added: Added, finished: bool) {
    let _ = STOPS.try_with(|s| s.borrow_mut().push(Stop { added, finished }));
    let _ = STATE.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            s.implicit = true;
        }
    });
}

/// Report stopped animations: their groups count them first (so completion
/// blocks run ahead of the delegates, as measured), then their delegates
/// hear.
fn report_stops(stops: Vec<Stop>) {
    for stop in &stops {
        for g in &stop.added.groups {
            g.done();
        }
    }
    for stop in stops {
        tell_stopped(&stop.added, stop.finished);
    }
}

/// An animation's delegate hears it stopped, from the run loop.
fn tell_stopped(added: &Added, finished: bool) {
    let Some(delegate) = super::animation::delegate_of(&added.object) else { return };
    // SAFETY: -respondsToSelector: takes a selector.
    let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel!(animationDidStop:finished:)] };
    if responds {
        let anim = added.object.clone();
        later(move || {
            // SAFETY: the delegate method takes the animation and a BOOL.
            let _: () = unsafe { msg_send![&*delegate, animationDidStop: &*anim, finished: finished] };
        });
    }
}

/// An animation's delegate hears it started, from the run loop.
fn tell_started(anim: &CAAnimation) {
    let Some(delegate) = super::animation::delegate_of(anim) else { return };
    // SAFETY: -respondsToSelector: takes a selector.
    let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel!(animationDidStart:)] };
    if responds {
        let anim = anim.retain();
        later(move || {
            // SAFETY: the delegate method takes the animation.
            let _: () = unsafe { msg_send![&*delegate, animationDidStart: &*anim] };
        });
    }
}

/// A layer that went to the render thread is gone.
pub(crate) fn layer_gone(id: LayerId) {
    let _ = GONE.try_with(|g| g.borrow_mut().push(id));
}

// The transactions.

pub(crate) fn begin() {
    with(|s| s.stack.push(Frame::default()));
}

/// Set the innermost transaction's duration (`NSAnimationContext`'s).
pub(crate) fn set_duration(d: f64) {
    ensure_frame(|f| f.duration = Some(d));
}

/// Set the innermost transaction's timing function.
pub(crate) fn set_function(function: Option<Retained<CAMediaTimingFunction>>) {
    ensure_frame(|f| f.function = Some(function));
}

pub(crate) fn commit() {
    let depth = with(|s| s.stack.len());
    if depth == 0 {
        return;
    }
    // The outermost commits with its settings still in force (its
    // disabled actions hold for the layout it runs, measured).
    if depth == 1 {
        commit_now();
    }
    let frame = with(|s| s.stack.pop());
    // Its completion blocks run once their animations are done (after the
    // commit, from the run loop, when they have none).
    if let Some(frame) = frame {
        frame.close();
    }
}

fn flush() {
    if with(|s| s.stack.is_empty()) {
        commit_now();
    }
}

fn end_of_turn() {
    if !with(|s| s.stack.is_empty()) {
        return;
    }
    if with(|s| s.implicit) {
        commit_now();
    }
    // The implicit transaction ends.
    let frame = with(|s| s.implicit_frame.take());
    if let Some(frame) = frame {
        frame.close();
    }
}

/// Resets the commit flag however a commit ends (program code it runs may
/// raise).
struct Committing;

impl Drop for Committing {
    fn drop(&mut self) {
        COMMITTING.with(|c| c.set(false));
    }
}

/// Commit every layer changed since the last commit.
pub(crate) fn commit_now() {
    if COMMITTING.with(|c| c.replace(true)) {
        return;
    }
    let guard = Committing;
    let now = math::media_now();
    let mut updates = Vec::new();
    let mut started: Vec<Retained<CAAnimation>> = Vec::new();
    let mut dropped: Vec<(Added, bool)> = Vec::new();
    // Layout and display may change more layers: go round again for those,
    // a few times (a layer that changes itself every time waits for the
    // next commit).
    for _ in 0..8 {
        let dirty = with(|s| {
            s.implicit = false;
            std::mem::take(&mut s.dirty)
        });
        if dirty.is_empty() {
            break;
        }
        let windows: Vec<Option<Retained<objc2_app_kit::NSWindow>>> = dirty
            .iter()
            .map(|l| {
                super::backing::window_of_root(&layer::root_of(imp(l))).filter(|w| super::backing::window_backed(w))
            })
            .collect();
        for (l, window) in dirty.iter().zip(&windows) {
            layer_out(l, window.is_some());
        }
        for (l, window) in dirty.iter().zip(windows) {
            let li = imp(l);
            li.ivars().dirty.set(false);
            let Some(window) = window else {
                // In no window's tree: its animations (and its tree's)
                // stop unfinished, as macOS stops them.
                drop_animations(l, &mut dropped);
                continue;
            };
            li.ivars().live.set(true);
            li.write(|m| m.committed = Some(m.props.clone()));
            // Its presentation shows what was committed from now on.
            li.ivars().presentation.borrow_mut().take();
            start_animations(li, now, &mut started);
            if super::backing::window_on_screen(&window) {
                updates.push(super::tree::update_of(li));
                li.ivars().sent.set(true);
            }
        }
    }
    drop(guard);
    let gone = GONE.with(|g| std::mem::take(&mut *g.borrow_mut()));
    let sent = updates.len();
    if !updates.is_empty() || !gone.is_empty() {
        super::tree::send_commit(updates, gone, now);
    }
    super::backing::committed();
    schedule_ending();
    if sent > 0 && crate::layers::tracing() {
        eprintln!("sidestep ca commit: main: {:.3} ms, {sent} layers", (math::media_now() - now) * 1000.0);
    }
    // After the commit returns, from the run loop: the delegates of the
    // animations that started, then of those that stopped since the last
    // commit.
    for anim in &started {
        tell_started(anim);
    }
    // Animations of layers in no window's tree: started, stopped, then
    // their groups (measured).
    for (added, was_started) in &dropped {
        if !was_started {
            tell_started(&added.object);
        }
        tell_stopped(added, false);
    }
    for (added, _) in &dropped {
        for g in &added.groups {
            g.done();
        }
    }
    let stops = STOPS.with(|s| std::mem::take(&mut *s.borrow_mut()));
    report_stops(stops);
}

/// Take the animations off a layer in no window's tree, noting each and
/// whether it had started (a layer leaving a tree has the layers under it
/// with animations marked too: [`detached`]).
fn drop_animations(l: &CALayer, out: &mut Vec<(Added, bool)>) {
    let li = imp(l);
    let gone: Vec<Added> = li.write(|m| std::mem::take(&mut m.anims));
    if gone.is_empty() {
        return;
    }
    li.ivars().presentation.borrow_mut().take();
    out.extend(gone.into_iter().rev().filter(|a| !a.ended).map(|a| {
        let started = a.started;
        (a, started)
    }));
}

/// `layer` left its tree: it and the layers under it that have animations
/// go to the next commit, which stops those animations if the layer is in
/// no window's tree by then.
pub(crate) fn detached(layer: &CALayerImpl) {
    mark_dirty(layer);
    for l in layer::tree(layer::as_layer(layer)).iter().skip(1) {
        if imp(l).read(|m| !m.anims.is_empty()) {
            mark_dirty(imp(l));
        }
    }
}

/// Lay out and display a changed layer as its commit does: a view's layer
/// only in a window, and only when its view updates its layer itself
/// (other views draw theirs in the display pass).
fn layer_out(l: &CALayer, in_window: bool) {
    let li = imp(l);
    if li.read(|m| m.needs_layout) {
        // SAFETY: -layoutIfNeeded takes nothing.
        let _: () = unsafe { msg_send![l, layoutIfNeeded] };
    }
    if !li.read(|m| m.needs_display) {
        return;
    }
    let displays = match li.view() {
        None => true,
        Some(view) => in_window && super::backing::updates_layer(&view),
    };
    if displays {
        // SAFETY: -displayIfNeeded takes nothing.
        let _: () = unsafe { msg_send![l, displayIfNeeded] };
    }
}

/// Settle the begin times of a live layer's new animations (0 becomes the
/// layer's time now), and note them to tell their delegates they started
/// (not on a paused layer, whose animations haven't, measured).
fn start_animations(layer: &CALayerImpl, now: f64, started: &mut Vec<Retained<CAAnimation>>) {
    if !layer.read(|m| m.anims.iter().any(|a| !a.started)) {
        return;
    }
    // A layer whose time is before 0 starts them at 0 (measured).
    let local = layer::local_time(layer, now).max(0.0);
    let frozen = layer::time_frozen(layer);
    layer.write(|m| {
        // The latest added first (measured).
        for added in m.anims.iter_mut().rev().filter(|a| !a.started) {
            added.started = true;
            if added.spec.timing.begin == 0.0 {
                let mut spec = (*added.spec).clone();
                spec.timing.begin = local;
                super::animation::settle_begin(&added.object, local);
                added.spec = std::sync::Arc::new(spec);
            }
            if !frozen {
                started.push(added.object.clone());
            }
        }
    });
    let weak = Weak::new(layer::as_layer(layer));
    ANIMATING.with(|a| a.borrow_mut().push(weak));
}

/// Set the timer for the earliest end among started animations still
/// running, or for the earliest begin time still to come (the render
/// thread, which doesn't draw frames for animations yet to begin, is woken
/// then).
fn schedule_ending() {
    let now = math::media_now();
    let mut earliest: Option<f64> = None;
    ANIMATING.with(|a| {
        let mut a = a.borrow_mut();
        let mut seen: HashSet<*const CALayer> = HashSet::new();
        a.retain(|w| {
            let Some(l) = w.load() else { return false };
            layer::animating(imp(&l)) && seen.insert(&*l as *const CALayer)
        });
        for w in a.iter() {
            let Some(l) = w.load() else { continue };
            let li = imp(&l);
            let times: Vec<(f64, bool)> = li.read(|m| {
                m.anims
                    .iter()
                    .filter(|a| a.started && !a.ended)
                    .flat_map(|a| [a.spec.end().map(|e| (e, true)), Some((a.spec.timing.begin, false))])
                    .flatten()
                    .collect()
            });
            for (t, is_end) in times {
                // Every end (one already past ends at once); begins still
                // to come.
                if let Some(t) = layer::to_media(li, t).filter(|t| is_end || *t > now) {
                    earliest = Some(earliest.map_or(t, |e: f64| e.min(t)));
                }
            }
        }
    });
    let Some(at) = earliest.filter(|t| t.is_finite()) else {
        ENDING.with(|e| {
            if let Some((_, timer)) = e.borrow_mut().take() {
                timer.invalidate();
            }
        });
        return;
    };
    let same = ENDING.with(|e| e.borrow().as_ref().is_some_and(|(t, _)| (*t - at).abs() < 1e-6));
    if same {
        return;
    }
    let delay = (at - now).max(0.0);
    let block = RcBlock::new(|_timer: std::ptr::NonNull<NSTimer>| {
        ENDING.with(|e| e.borrow_mut().take());
        // Animations may begin now: the render thread draws them.
        super::tree::wake();
        end_animations();
    });
    // SAFETY: the block owns what it calls; the timer copies it.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(delay, false, &block) };
    // SAFETY: adding a timer to the current run loop in the common modes.
    unsafe {
        let rl = objc2_foundation::NSRunLoop::currentRunLoop();
        rl.addTimer_forMode(&timer, objc2_foundation::NSRunLoopCommonModes);
    }
    ENDING.with(|e| {
        if let Some((_, old)) = e.borrow_mut().replace((at, timer)) {
            old.invalidate();
        }
    });
}

/// End the animations whose time is up: those removed on completion go,
/// the others stay, ended; either way their groups count them, then their
/// delegates hear they finished.
fn end_animations() {
    let layers: Vec<Retained<CALayer>> = ANIMATING.with(|a| a.borrow().iter().filter_map(Weak::load).collect());
    let now = math::media_now();
    let mut stops = Vec::new();
    for l in layers {
        let li = imp(&l);
        let t = layer::local_time(li, now);
        let ended: Vec<Added> = li.write(|m| {
            let mut ended = Vec::new();
            let mut kept = Vec::new();
            for mut added in std::mem::take(&mut m.anims) {
                let over = added.started && !added.ended && added.spec.end().is_some_and(|end| t >= end - 1e-9);
                if over && added.spec.removed_on_completion {
                    ended.push(added);
                } else if over {
                    // Kept (it fills), but done: its delegate and groups
                    // hear once, and nothing waits for it again.
                    added.ended = true;
                    ended.push(Added {
                        key: added.key.clone(),
                        object: added.object.clone(),
                        spec: added.spec.clone(),
                        started: true,
                        ended: true,
                        groups: std::mem::take(&mut added.groups),
                    });
                    kept.push(added);
                } else {
                    kept.push(added);
                }
            }
            m.anims = kept;
            ended
        });
        if !ended.is_empty() {
            mark_dirty(li);
        }
        stops.extend(ended.into_iter().rev().map(|added| Stop { added, finished: true }));
    }
    report_stops(stops);
    schedule_ending();
}

// The class.

define_class!(
    // SAFETY: NSObject has no subclassing requirements; transactions are
    // per thread and kept here, not in instances.
    #[unsafe(super(NSObject))]
    #[name = "CATransaction"]
    pub(crate) struct CATransactionImpl;

    impl CATransactionImpl {
        #[unsafe(method(begin))]
        fn begin_class() {
            begin();
        }

        #[unsafe(method(commit))]
        fn commit_class() {
            commit();
        }

        #[unsafe(method(flush))]
        fn flush_class() {
            flush();
        }

        #[unsafe(method(lock))]
        fn lock_class() {
            lock();
        }

        #[unsafe(method(unlock))]
        fn unlock_class() {
            unlock();
        }

        #[unsafe(method(animationDuration))]
        fn animation_duration_class() -> f64 {
            animation_duration()
        }

        #[unsafe(method(setAnimationDuration:))]
        fn set_animation_duration(d: f64) {
            ensure_frame(|f| f.duration = Some(d));
        }

        #[unsafe(method_id(animationTimingFunction))]
        fn animation_timing_function() -> Option<Retained<CAMediaTimingFunction>> {
            timing_function()
        }

        #[unsafe(method(setAnimationTimingFunction:))]
        fn set_animation_timing_function(f: Option<&CAMediaTimingFunction>) {
            let f = f.map(Message::retain);
            ensure_frame(|frame| frame.function = Some(f));
        }

        #[unsafe(method(disableActions))]
        fn disable_actions_class() -> bool {
            disable_actions()
        }

        #[unsafe(method(setDisableActions:))]
        fn set_disable_actions(flag: bool) {
            ensure_frame(|f| f.disable = Some(flag));
        }

        #[unsafe(method(completionBlock))]
        fn completion_block() -> *mut DynBlock<dyn Fn()> {
            with(|s| {
                s.stack
                    .last()
                    .or(s.implicit_frame.as_ref())
                    .and_then(|f| f.group.as_ref())
                    .and_then(|g| g.block.borrow().as_ref().map(RcBlock::as_ptr))
                    .unwrap_or(std::ptr::null_mut())
            })
        }

        #[unsafe(method(setCompletionBlock:))]
        fn set_completion_block(block: Option<&DynBlock<dyn Fn()>>) {
            let block = block.map(DynBlock::copy);
            set_completion(block);
        }

        #[unsafe(method_id(valueForKey:))]
        fn value_for_key(key: &NSString) -> Option<Retained<AnyObject>> {
            let k = key.to_string();
            match k.as_str() {
                "animationDuration" => Some(super::objects::number(animation_duration())),
                "disableActions" => Some(super::objects::boolean(disable_actions())),
                "animationTimingFunction" => timing_function().map(super::objects::any),
                _ => lookup(|f| f.values.iter().rev().find(|(n, _)| *n == k).map(|(_, v)| v.clone())),
            }
        }

        #[unsafe(method(setValue:forKey:))]
        fn set_value_for_key(value: Option<&AnyObject>, key: &NSString) {
            let k = key.to_string();
            let number = |v: Option<&AnyObject>| -> f64 {
                // SAFETY: numbers answer -doubleValue.
                v.map_or(0.0, |v| unsafe { msg_send![v, doubleValue] })
            };
            match k.as_str() {
                "animationDuration" => ensure_frame(|f| f.duration = Some(number(value))),
                "disableActions" => ensure_frame(|f| f.disable = Some(number(value) != 0.0)),
                "animationTimingFunction" => {
                    let f = value.and_then(|v| v.downcast_ref::<CAMediaTimingFunction>()).map(Message::retain);
                    ensure_frame(|frame| frame.function = Some(f));
                }
                _ => {
                    let value = value.map(Message::retain);
                    ensure_frame(|f| {
                        f.values.retain(|(n, _)| *n != k);
                        if let Some(v) = value {
                            f.values.push((k.clone(), v));
                        }
                    });
                }
            }
        }
    }
);

/// Change the innermost transaction: outside any explicit one, the
/// implicit one, which lasts until the end of the turn.
fn ensure_frame(f: impl FnOnce(&mut Frame)) {
    let implicit = with(|s| match s.stack.last_mut() {
        Some(frame) => {
            f(frame);
            false
        }
        None => {
            f(s.implicit_frame.get_or_insert_with(Frame::default));
            true
        }
    });
    if implicit {
        install_observer();
    }
}

/// `setCompletionBlock:`: a new completion group for the animations added
/// from now on; the one it replaces still runs its block when its own
/// animations are done (measured).
fn set_completion(block: Option<RcBlock<dyn Fn()>>) {
    ensure_frame(|f| {
        if let Some(old) = f.group.replace(Group::new(block)) {
            f.replaced.push(old);
        }
    });
}

// `+lock` and `+unlock`: one lock for the process, taken again by the
// thread holding it.

static LOCK: std::sync::Mutex<(Option<std::thread::ThreadId>, usize)> = std::sync::Mutex::new((None, 0));
static FREED: std::sync::Condvar = std::sync::Condvar::new();

fn lock() {
    let me = std::thread::current().id();
    let mut l = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        match l.0 {
            None => {
                *l = (Some(me), 1);
                return;
            }
            Some(owner) if owner == me => {
                l.1 += 1;
                return;
            }
            Some(_) => l = FREED.wait(l).unwrap_or_else(|e| e.into_inner()),
        }
    }
}

fn unlock() {
    let me = std::thread::current().id();
    let mut l = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if l.0 == Some(me) {
        l.1 -= 1;
        if l.1 == 0 {
            l.0 = None;
            FREED.notify_one();
        }
    }
}

// For tests.

/// Whether a timer waits to end (or begin) an animation.
pub(crate) fn ending_scheduled() -> bool {
    ENDING.with(|e| e.borrow().is_some())
}
