//! `NSAnimationContext` and `animator`.
//!
//! A change made through a view's or a window's `animator` (which is the
//! view or window itself, as objc2 types it) applies at once, as AppKit's
//! do with a duration of 0. A group is also a Core Animation transaction
//! with its duration and timing function, so layers the program changes
//! in it animate over them, and so do layer-backed views' changes while
//! `allowsImplicitAnimation` is set (`quartzcore::backing`). What programs
//! can see of grouping behaves as AppKit's: each thread has one
//! context whose settings (duration, implicit animation, completion
//! handler) are saved and restored around each group, a group starts with
//! its enclosing group's settings, and completion handlers never run
//! inside the group. They run once its duration has passed, from a timer
//! on the thread's run loop, so a program that waits for one (to remove a
//! view it faded out, or to chain animations) keeps its order and timing.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, define_class, msg_send};
use objc2_app_kit::NSAnimationContext;
use objc2_foundation::{NSTimeInterval, NSTimer};

sidestep_runtime::static_class!(pub NSANIMATIONCONTEXT, NSANIMATIONCONTEXT_META = "NSAnimationContext", || {
    let _ = NSAnimationContextImpl::class();
});

type Completion = RcBlock<dyn Fn()>;

/// A group's settings.
#[derive(Clone)]
struct Group {
    duration: NSTimeInterval,
    implicit: bool,
    completion: Option<Completion>,
    function: Option<Retained<objc2_quartz_core::CAMediaTimingFunction>>,
}

impl Default for Group {
    fn default() -> Self {
        // AppKit's defaults.
        Group { duration: 0.25, implicit: false, completion: None, function: None }
    }
}

/// Whether the current group allows implicit animation (of views'
/// layers: `quartzcore::backing`).
pub(crate) fn allows_implicit() -> bool {
    STATE.with(|s| s.try_borrow().map(|s| s.1.last().is_some_and(|g| g.implicit) && s.1.len() > 1).unwrap_or(false))
}

thread_local! {
    /// The thread's context and the groups open on it, innermost last
    /// (the first is outside any group).
    static STATE: RefCell<(Option<Retained<NSAnimationContext>>, Vec<Group>)> =
        RefCell::new((None, vec![Group::default()]));
}

/// Run `f` on the innermost group's settings.
fn with_group<R>(f: impl FnOnce(&mut Group) -> R) -> R {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let group = s.1.last_mut().expect("the outermost group is never closed");
        f(group)
    })
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the context's state
    // lives in a thread local of the thread it belongs to.
    #[unsafe(super(NSObject))]
    #[name = "NSAnimationContext"]
    pub(crate) struct NSAnimationContextImpl;

    impl NSAnimationContextImpl {
        #[unsafe(method_id(currentContext))]
        fn current_context() -> Retained<NSAnimationContext> {
            current()
        }

        #[unsafe(method(runAnimationGroup:completionHandler:))]
        fn run_group_completion(
            changes: &DynBlock<dyn Fn(NonNull<NSAnimationContext>) + '_>,
            completion: Option<&DynBlock<dyn Fn()>>,
        ) {
            run_group(changes, completion);
        }

        #[unsafe(method(runAnimationGroup:))]
        fn run_group_only(changes: &DynBlock<dyn Fn(NonNull<NSAnimationContext>) + '_>) {
            run_group(changes, None);
        }

        #[unsafe(method(beginGrouping))]
        fn begin_grouping() {
            begin();
        }

        #[unsafe(method(endGrouping))]
        fn end_grouping() {
            end(None);
        }

        #[unsafe(method(duration))]
        fn duration(&self) -> NSTimeInterval {
            with_group(|g| g.duration)
        }

        #[unsafe(method(setDuration:))]
        fn set_duration(&self, duration: NSTimeInterval) {
            // Kept as given, as AppKit keeps it; a negative one is none.
            with_group(|g| g.duration = duration);
            // A group is a Core Animation transaction: its animations take
            // its duration.
            if in_group() {
                crate::quartzcore::transaction::set_duration(duration.max(0.0));
            }
        }

        #[unsafe(method_id(timingFunction))]
        fn timing_function(&self) -> Option<Retained<objc2_quartz_core::CAMediaTimingFunction>> {
            with_group(|g| g.function.clone())
        }

        #[unsafe(method(setTimingFunction:))]
        fn set_timing_function(&self, function: Option<&objc2_quartz_core::CAMediaTimingFunction>) {
            let function = function.map(objc2::Message::retain);
            with_group(|g| g.function = function.clone());
            if in_group() {
                crate::quartzcore::transaction::set_function(function);
            }
        }

        #[unsafe(method(allowsImplicitAnimation))]
        fn allows_implicit_animation(&self) -> bool {
            with_group(|g| g.implicit)
        }

        #[unsafe(method(setAllowsImplicitAnimation:))]
        fn set_allows_implicit_animation(&self, flag: bool) {
            with_group(|g| g.implicit = flag);
        }

        #[unsafe(method(completionHandler))]
        fn completion_handler(&self) -> *mut DynBlock<dyn Fn()> {
            with_group(|g| g.completion.as_ref().map_or(std::ptr::null_mut(), RcBlock::as_ptr))
        }

        #[unsafe(method(setCompletionHandler:))]
        fn set_completion_handler(&self, handler: Option<&DynBlock<dyn Fn()>>) {
            let handler = handler.map(DynBlock::copy);
            with_group(|g| g.completion = handler);
        }
    }

    unsafe impl NSObjectProtocol for NSAnimationContextImpl {}
);

/// The thread's context, made on first use.
fn current() -> Retained<NSAnimationContext> {
    if let Some(c) = STATE.with(|s| s.borrow().0.clone()) {
        return c;
    }
    crate::load_shell::<NSAnimationContext>();
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSAnimationContextImpl> = unsafe { msg_send![NSAnimationContextImpl::alloc(), init] };
    // SAFETY: NSAnimationContextImpl is the class NSAnimationContext names.
    let this: Retained<NSAnimationContext> = unsafe { Retained::cast_unchecked(this) };
    STATE.with(|s| s.borrow_mut().0 = Some(this.clone()));
    this
}

/// Open a group with the enclosing one's settings, but no completion: a
/// Core Animation transaction with its duration and timing function.
fn begin() {
    let (duration, function) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let inner = Group { completion: None, ..s.1.last().cloned().unwrap_or_default() };
        let settings = (inner.duration, inner.function.clone());
        s.1.push(inner);
        settings
    });
    crate::quartzcore::transaction::begin();
    crate::quartzcore::transaction::set_duration(duration.max(0.0));
    if function.is_some() {
        crate::quartzcore::transaction::set_function(function);
    }
}

/// Whether a group is open.
fn in_group() -> bool {
    STATE.with(|s| s.borrow().1.len() > 1)
}

/// Close the innermost group, scheduling its completion handler (and
/// `extra`) for when its duration has passed.
fn end(extra: Option<Completion>) {
    let group = STATE.with(|s| {
        let mut s = s.borrow_mut();
        // The outermost settings stay: ending more groups than began ends
        // nothing.
        if s.1.len() > 1 { s.1.pop() } else { None }
    });
    if group.is_some() {
        crate::quartzcore::transaction::commit();
    }
    let duration = group.as_ref().map_or(0.0, |g| g.duration.max(0.0));
    for handler in group.and_then(|g| g.completion).into_iter().chain(extra) {
        schedule(duration, handler);
    }
}

fn run_group(changes: &DynBlock<dyn Fn(NonNull<NSAnimationContext>) + '_>, completion: Option<&DynBlock<dyn Fn()>>) {
    let context = current();
    begin();
    changes.call((NonNull::from(&*context),));
    end(completion.map(DynBlock::copy));
}

/// Run `handler` from the run loop after `delay` seconds (at the next turn
/// for none).
fn schedule(delay: NSTimeInterval, handler: Completion) {
    let block = RcBlock::new(move |_timer: NonNull<NSTimer>| handler.call(()));
    // SAFETY: the block owns what it calls, and the timer copies it.
    let _ = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(delay, false, &block) };
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use sidestep_foundation::runloop::{self, Mode};

    use super::*;

    #[test]
    fn completions_wait_for_their_duration_on_the_run_loop() {
        let done = Rc::new(Cell::new(0));
        let d = done.clone();
        let changes = RcBlock::new(|c: NonNull<NSAnimationContext>| {
            // SAFETY: the group passes the context.
            unsafe { c.as_ref() }.setDuration(0.2);
        });
        let completion = RcBlock::new(move || d.set(d.get() + 1));
        // From before the group, so a slow machine can't make the wait
        // look shorter than the duration.
        let start = Instant::now();
        NSAnimationContext::runAnimationGroup_completionHandler(&changes, Some(&completion));
        assert_eq!(done.get(), 0, "not within the group");
        let run_loop = runloop::current();
        run_loop.run_mode(Mode::DEFAULT, Some(Instant::now()), false);
        assert_eq!(done.get(), 0, "not before its duration");
        while done.get() == 0 && start.elapsed() < Duration::from_secs(5) {
            run_loop.run_mode(Mode::DEFAULT, Some(Instant::now() + Duration::from_millis(20)), false);
        }
        assert_eq!(done.get(), 1);
        assert!(start.elapsed() >= Duration::from_millis(190), "{:?}", start.elapsed());
    }
}
