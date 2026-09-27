//! `CADisplayLink`: a callback each frame a window shows.
//!
//! A link made by a view or a window (`displayLinkWithTarget:selector:`,
//! macOS 14) follows that window's frames: while it's in a run loop and
//! not paused, the render thread asks the compositor for a frame callback
//! each frame (`ToRender::FrameTicks`) and reports each
//! (`FromRender::Tick`), and the main thread calls the link's target, in
//! the run loop modes the link was added to. A window the compositor
//! isn't showing gets no frame callbacks, so its links wait, as macOS's
//! wait while a window can't show. A link made by a screen ticks from a
//! timer at the rate the screen refreshes at (`maximumFramesPerSecond`),
//! and one made by the class (`+displayLinkWithTarget:selector:`, which
//! macOS doesn't offer) at 60 Hz. As on macOS, a link in a run loop is
//! kept alive until it is invalidated (the program needn't keep it), and
//! it retains its target until then; its `duration` is 0 until its first
//! frame.

use std::cell::{Cell, RefCell};
use std::ffi::c_float;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::CFTimeInterval;
use objc2_foundation::{NSInteger, NSRunLoop, NSString, NSTimer};
use objc2_quartz_core::{CADisplayLink, CAFrameRateRange};
use sidestep_foundation::runloop::{self, Mode};

use crate::protocol::{ToRender, WindowId};

/// The frame interval links without a window assume: 60 Hz.
const FRAME: f64 = 1.0 / 60.0;

pub(crate) struct LinkIvars {
    target: RefCell<Option<Retained<AnyObject>>>,
    selector: Cell<Option<Sel>>,
    /// The window whose frames it follows, if it's a window's link.
    window_ref: RefCell<Weak<objc2_app_kit::NSWindow>>,
    /// A view's link follows whatever window the view is in.
    view_ref: RefCell<Weak<objc2_app_kit::NSView>>,
    modes: RefCell<Vec<Mode>>,
    paused: Cell<bool>,
    valid: Cell<bool>,
    timestamp: Cell<f64>,
    /// The frame interval reported (0 before the first frame).
    duration: Cell<f64>,
    /// The frame interval it ticks at.
    period: Cell<f64>,
    interval: Cell<NSInteger>,
    preferred_fps: Cell<NSInteger>,
    range: Cell<(f32, f32, f32)>,
    timer: RefCell<Option<Retained<NSTimer>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a link belongs to
    // the main thread.
    #[unsafe(super(NSObject))]
    #[name = "CADisplayLink"]
    #[ivars = LinkIvars]
    pub(crate) struct CADisplayLinkImpl;

    impl CADisplayLinkImpl {
        #[unsafe(method_id(displayLinkWithTarget:selector:))]
        fn display_link_with_target(target: &AnyObject, selector: Sel) -> Retained<CADisplayLink> {
            new_link(target, selector, None)
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ivars(None, None));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(addToRunLoop:forMode:))]
        fn add_to_run_loop(&self, _run_loop: &NSRunLoop, mode: &NSString) {
            if !self.ivars().valid.get() {
                return;
            }
            let mode = Mode::from_ns(mode);
            let added = {
                let mut modes = self.ivars().modes.borrow_mut();
                let fresh = modes.is_empty();
                if !modes.contains(&mode) {
                    modes.push(mode);
                }
                fresh
            };
            if added {
                start(self);
            } else if let Some(timer) = self.ivars().timer.borrow().as_ref() {
                // SAFETY: adding a timer to the main run loop in a mode.
                unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(timer, mode.name()) };
            }
        }

        #[unsafe(method(removeFromRunLoop:forMode:))]
        fn remove_from_run_loop(&self, _run_loop: &NSRunLoop, mode: &NSString) {
            let mode = Mode::from_ns(mode);
            let empty = {
                let mut modes = self.ivars().modes.borrow_mut();
                modes.retain(|m| *m != mode);
                modes.is_empty()
            };
            if empty {
                stop(self);
            }
        }

        #[unsafe(method(invalidate))]
        fn invalidate(&self) {
            self.ivars().valid.set(false);
            self.ivars().modes.borrow_mut().clear();
            stop(self);
            let target = self.ivars().target.borrow_mut().take();
            drop(target);
        }

        #[unsafe(method(timestamp))]
        fn timestamp(&self) -> CFTimeInterval {
            self.ivars().timestamp.get()
        }

        #[unsafe(method(duration))]
        fn duration(&self) -> CFTimeInterval {
            self.ivars().duration.get()
        }

        #[unsafe(method(targetTimestamp))]
        fn target_timestamp(&self) -> CFTimeInterval {
            self.ivars().timestamp.get() + self.ivars().period.get()
        }

        #[unsafe(method(isPaused))]
        fn is_paused(&self) -> bool {
            self.ivars().paused.get()
        }

        #[unsafe(method(setPaused:))]
        fn set_paused(&self, paused: bool) {
            if self.ivars().paused.replace(paused) != paused {
                if paused {
                    stop(self);
                } else if !self.ivars().modes.borrow().is_empty() {
                    start(self);
                }
            }
        }

        #[unsafe(method(frameInterval))]
        fn frame_interval(&self) -> NSInteger {
            self.ivars().interval.get()
        }

        #[unsafe(method(setFrameInterval:))]
        fn set_frame_interval(&self, interval: NSInteger) {
            self.ivars().interval.set(interval.max(1));
        }

        #[unsafe(method(preferredFramesPerSecond))]
        fn preferred_frames_per_second(&self) -> NSInteger {
            self.ivars().preferred_fps.get()
        }

        #[unsafe(method(setPreferredFramesPerSecond:))]
        fn set_preferred_frames_per_second(&self, fps: NSInteger) {
            self.ivars().preferred_fps.set(fps.max(0));
        }

        #[unsafe(method(preferredFrameRateRange))]
        fn preferred_frame_rate_range(&self) -> CAFrameRateRange {
            let (minimum, maximum, preferred) = self.ivars().range.get();
            CAFrameRateRange { minimum, maximum, preferred }
        }

        #[unsafe(method(setPreferredFrameRateRange:))]
        fn set_preferred_frame_rate_range(&self, range: CAFrameRateRange) {
            self.ivars().range.set((range.minimum as c_float, range.maximum, range.preferred));
        }
    }
);

fn ivars(target: Option<&AnyObject>, selector: Option<Sel>) -> LinkIvars {
    LinkIvars {
        target: RefCell::new(target.map(Message::retain)),
        selector: Cell::new(selector),
        window_ref: RefCell::new(Weak::default()),
        view_ref: RefCell::new(Weak::default()),
        modes: RefCell::new(Vec::new()),
        paused: Cell::new(false),
        valid: Cell::new(true),
        timestamp: Cell::new(0.0),
        duration: Cell::new(0.0),
        period: Cell::new(FRAME),
        interval: Cell::new(1),
        preferred_fps: Cell::new(0),
        range: Cell::new((0.0, 0.0, 0.0)),
        timer: RefCell::new(None),
    }
}

/// What a link follows the frames of.
pub(crate) enum Owner<'a> {
    View(&'a objc2_app_kit::NSView),
    Window(&'a objc2_app_kit::NSWindow),
    /// A screen, by the frames a second it refreshes at.
    Screen(isize),
}

/// A new link calling `selector` on `target`, following its owner's
/// window's frames.
pub(crate) fn new_link(target: &AnyObject, selector: Sel, owner: Option<Owner>) -> Retained<CADisplayLink> {
    crate::load_shell::<CADisplayLink>();
    let this = CADisplayLinkImpl::alloc().set_ivars(ivars(Some(target), Some(selector)));
    // SAFETY: NSObject's designated initializer.
    let this: Retained<CADisplayLinkImpl> = unsafe { msg_send![super(this), init] };
    match owner {
        Some(Owner::View(v)) => *this.ivars().view_ref.borrow_mut() = Weak::new(v),
        Some(Owner::Window(w)) => *this.ivars().window_ref.borrow_mut() = Weak::new(w),
        Some(Owner::Screen(fps)) => this.ivars().period.set(1.0 / fps.max(1) as f64),
        None => {}
    }
    // SAFETY: the class is CADisplayLink.
    unsafe { Retained::cast_unchecked(this) }
}

/// Whether a link follows a window's frames (a view's or a window's link).
fn follows_window(link: &CADisplayLinkImpl) -> bool {
    link.ivars().window_ref.borrow().load().is_some() || link.ivars().view_ref.borrow().load().is_some()
}

thread_local! {
    /// Links following windows' frames (kept alive while in a run loop),
    /// and the windows that tick.
    static LINKS: RefCell<Vec<Retained<CADisplayLinkImpl>>> = const { RefCell::new(Vec::new()) };
    static TICKING: RefCell<Vec<WindowId>> = const { RefCell::new(Vec::new()) };
}

/// The window a link follows, if it's on screen now.
fn window_id(link: &CADisplayLinkImpl) -> Option<WindowId> {
    let w = match link.ivars().view_ref.borrow().load() {
        Some(v) => crate::views::window_of(crate::views::imp(&v)).map(|w| w.as_window().retain())?,
        None => link.ivars().window_ref.borrow().load()?,
    };
    let wi = crate::window::imp(&w);
    wi.on_screen().then(|| wi.id())
}

fn start(link: &CADisplayLinkImpl) {
    if link.ivars().paused.get() || !link.ivars().valid.get() {
        return;
    }
    if follows_window(link) {
        LINKS.with(|l| {
            let mut l = l.borrow_mut();
            if !l.iter().any(|x| std::ptr::eq(&**x, link)) {
                l.push(link.retain());
            }
        });
        update_ticks();
        return;
    }
    // No window: a timer at the link's rate in its modes, which keeps the
    // link alive (the run loop keeps the timer) until it stops.
    let kept = link.retain();
    let block = block2::RcBlock::new(move |_t: std::ptr::NonNull<NSTimer>| {
        fire(&kept, crate::quartzcore::math::media_now());
    });
    // SAFETY: the block owns what it calls; the timer copies it.
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(link.ivars().period.get(), true, &block) };
    for mode in link.ivars().modes.borrow().iter() {
        // SAFETY: adding a timer to the main run loop in a mode.
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, mode.name()) };
    }
    if let Some(old) = link.ivars().timer.borrow_mut().replace(timer) {
        old.invalidate();
    }
}

fn stop(link: &CADisplayLinkImpl) {
    if let Some(timer) = link.ivars().timer.borrow_mut().take() {
        timer.invalidate();
    }
    // Taken out before it's dropped: its release may run program code.
    let gone: Vec<Retained<CADisplayLinkImpl>> = LINKS.with(|l| {
        let mut l = l.borrow_mut();
        let (gone, kept) = std::mem::take(&mut *l).into_iter().partition(|x| std::ptr::eq(&**x, link));
        *l = kept;
        gone
    });
    update_ticks();
    drop(gone);
}

/// Ask for frame ticks for the windows with active links, and stop them
/// for the others.
pub(crate) fn update_ticks() {
    let wanted: Vec<WindowId> = LINKS.with(|l| {
        let l = l.borrow();
        let mut ids: Vec<WindowId> = l.iter().filter_map(|x| window_id(x)).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    });
    let old = TICKING.with(|t| std::mem::replace(&mut *t.borrow_mut(), wanted.clone()));
    for id in &wanted {
        if !old.contains(id) {
            crate::app::send(ToRender::FrameTicks { window: *id, on: true });
        }
    }
    for id in old {
        if !wanted.contains(&id) {
            crate::app::send_if_running(ToRender::FrameTicks { window: id, on: false });
        }
    }
}

/// A frame of `window` showed at the media time `time`: call its links
/// that are in the mode the loop runs in.
pub(crate) fn tick(window: WindowId, time: f64) {
    let links: Vec<Retained<CADisplayLinkImpl>> = LINKS.with(|l| l.borrow().clone());
    let mode = runloop::main().current_mode().unwrap_or(Mode::DEFAULT);
    for link in links {
        if window_id(&link) != Some(window) || link.ivars().paused.get() {
            continue;
        }
        let modes = link.ivars().modes.borrow().clone();
        let common = runloop::main().is_common_mode(mode);
        if !modes.iter().any(|m| *m == mode || (*m == Mode::COMMON && common)) {
            continue;
        }
        fire(&link, time);
    }
}

fn fire(link: &CADisplayLinkImpl, time: f64) {
    link.ivars().timestamp.set(time);
    link.ivars().duration.set(link.ivars().period.get());
    let target = link.ivars().target.borrow().clone();
    let (Some(target), Some(sel)) = (target, link.ivars().selector.get()) else { return };
    // SAFETY: the target's action takes the link, as its selector says.
    let _: () = unsafe { objc2::runtime::MessageReceiver::send_message(&*target, sel, (link as &AnyObject,)) };
}

/// Links whose window came on screen or left it.
pub(crate) fn windows_changed() {
    update_ticks();
}
