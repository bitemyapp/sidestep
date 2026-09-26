//! Sending a selector chosen at run time, and the `NSObject` methods that
//! send one later or on another thread: `-performSelector:withObject:
//! afterDelay:` and its cancellation, `-performSelectorOnMainThread:…`,
//! `-performSelector:onThread:…` and `-performSelectorInBackground:…`.
//!
//! A delayed perform is a one-shot timer on the calling thread's run loop,
//! in the default mode unless modes are given; it retains the target and
//! argument until it fires or is cancelled, and cancelling matches the
//! argument with `-isEqual:` (nil only matches nil), as on macOS. A perform
//! on another thread's loop goes to that loop's perform source, in the
//! common modes (or the modes given), so, as on macOS, performing it ends a
//! `-runMode:beforeDate:`, and a burst of requests runs in one go; with
//! `waitUntilDone:` the caller waits for it, and a request for the calling
//! thread itself runs at once. A request for a thread that ends first is
//! dropped; a caller waiting for it panics with the reason macOS raises.
//!
//! The methods live on a helper class and are copied onto `NSObject` by
//! [`install`], which every Foundation class loader calls.

use std::sync::{Arc, Condvar, Mutex, Once};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject, Sel};
use objc2::{ClassType, Message, define_class, sel};

use crate::runloop::core::{self, Shared, with_state};
use crate::runloop::modes::Mode;
use crate::runloop::nsrunloop::block_modes;
use crate::runloop::timers;
use crate::thread::{lock, thread_impl};
use crate::timer::{Action, NSTimerImpl, Payload};

/// Send `selector` to `target` with one object argument, ignoring any
/// result, as `-performSelector:withObject:` does.
///
/// # Safety
/// The method must take one object argument (or none: the extra argument
/// is ignored by the C calling convention) and return an object or nothing.
pub(crate) unsafe fn send_object(target: &AnyObject, selector: Sel, argument: Option<&AnyObject>) {
    let receiver = (target as *const AnyObject).cast_mut();
    // SAFETY: a live receiver; lookup never fails (unknown selectors panic
    // inside it with the usual message).
    let imp = unsafe { objc2::ffi::objc_msg_lookup(receiver, selector) }.expect("lookup never fails");
    // SAFETY: guaranteed by the caller; methods returning an object leave
    // it in the return register, which is ignored.
    unsafe {
        let imp: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *const AnyObject) = std::mem::transmute(imp);
        imp(receiver, selector, argument.map_or(std::ptr::null(), |a| a as *const AnyObject));
    }
}

/// A message to send later, perhaps on another thread.
struct Request {
    target: Retained<AnyObject>,
    selector: Sel,
    argument: Option<Retained<AnyObject>>,
}

// SAFETY: sending a message from another thread is the caller's request,
// as with the Objective-C methods; reference counting is thread-safe.
unsafe impl Send for Request {}

impl Request {
    fn new(target: &AnyObject, selector: Sel, argument: Option<&AnyObject>) -> Self {
        Request { target: target.retain(), selector, argument: argument.map(|a| a.retain()) }
    }

    fn send(&self) {
        // SAFETY: performed selectors take one object argument.
        unsafe { send_object(&self.target, self.selector, self.argument.as_deref()) };
    }
}

/// Schedule a delayed perform on this thread's loop.
fn after_delay(target: &AnyObject, selector: Sel, argument: Option<&AnyObject>, delay: f64, modes: &[Mode]) {
    if modes.is_empty() {
        return;
    }
    crate::timer::load();
    let action = Action::Perform { target: target.retain(), selector, argument: argument.map(|a| a.retain()) };
    let timer = NSTimerImpl::make(
        crate::date::now() + delay.max(0.0),
        0.0,
        0,
        Payload { action: Some(action), user_info: None },
    );
    let shared = core::current_shared();
    for &mode in modes {
        timers::add(&shared, &timer, mode);
    }
}

/// Cancel this thread's delayed performs of `target` matching `selector`
/// and `argument` (`None`: any selector and argument).
fn cancel(target: &AnyObject, filter: Option<(Sel, Option<&AnyObject>)>) {
    let performs: Vec<Retained<NSTimerImpl>> = with_state(|s| {
        s.timers.values().filter(|e| e.timer.is_perform(target, None, None)).map(|e| e.timer.clone()).collect()
    });
    for timer in performs {
        let matches = match filter {
            None => true,
            Some((selector, argument)) => timer.is_perform(target, Some(selector), Some(argument)),
        };
        if matches {
            timers::invalidate(&timer);
        }
    }
}

/// How a request someone waits for ended: `None` while pending, then
/// whether it was performed (or dropped because its thread ended).
type Outcome = (Mutex<Option<bool>>, Condvar);

/// Tells the waiting caller when the request is done with, however that
/// happens: performed (even by unwinding), or dropped unperformed.
struct Signal {
    outcome: Arc<Outcome>,
    performed: bool,
}

impl Drop for Signal {
    fn drop(&mut self) {
        *lock(&self.outcome.0) = Some(self.performed);
        self.outcome.1.notify_all();
    }
}

/// Send the message on `shared`'s thread in `modes`, and with `wait` wait
/// until it has been sent. A request for the calling thread with `wait`
/// sends it at once.
fn perform_on(shared: &Arc<Shared>, message: Request, wait: bool, modes: Vec<Mode>) {
    if wait && shared.is_current() {
        message.send();
        return;
    }
    if modes.is_empty() {
        return;
    }
    if !wait {
        core::perform_as_source(shared, modes, Box::new(move || message.send()));
        return;
    }
    let class = message.target.class().name().to_string_lossy().into_owned();
    let outcome: Arc<Outcome> = Arc::new((Mutex::new(None), Condvar::new()));
    let signal = Signal { outcome: outcome.clone(), performed: false };
    core::perform_as_source(
        shared,
        modes,
        Box::new(move || {
            let mut signal = signal;
            signal.performed = true;
            message.send();
        }),
    );
    let mut state = lock(&outcome.0);
    let performed = loop {
        match *state {
            Some(performed) => break performed,
            None => state = outcome.1.wait(state).unwrap_or_else(|e| e.into_inner()),
        }
    };
    drop(state);
    assert!(
        performed,
        "*** -[{class} performSelector:onThread:withObject:waitUntilDone:modes:]: target thread exited while waiting \
         for the perform"
    );
}

/// The modes an optional array names; the common modes without one.
fn modes_or_common(modes: Option<&AnyObject>) -> Vec<Mode> {
    match modes {
        None => vec![Mode::COMMON],
        Some(array) => mode_list(array),
    }
}

fn mode_list(array: &AnyObject) -> Vec<Mode> {
    let set = block_modes(array);
    let mut modes: Vec<Mode> = Vec::new();
    if set.common {
        modes.push(Mode::COMMON);
    }
    modes.extend(set.iter());
    modes
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepPerforming"]
    struct Performing;

    impl Performing {
        #[unsafe(method(performSelector:withObject:afterDelay:))]
        fn perform_after_delay(&self, selector: Sel, argument: Option<&AnyObject>, delay: f64) {
            after_delay(this(self), selector, argument, delay, &[Mode::DEFAULT]);
        }

        #[unsafe(method(performSelector:withObject:afterDelay:inModes:))]
        fn perform_after_delay_in_modes(&self, selector: Sel, argument: Option<&AnyObject>, delay: f64, modes: &AnyObject) {
            after_delay(this(self), selector, argument, delay, &mode_list(modes));
        }

        #[unsafe(method(cancelPreviousPerformRequestsWithTarget:selector:object:))]
        fn cancel_matching(target: &AnyObject, selector: Sel, argument: Option<&AnyObject>) {
            cancel(target, Some((selector, argument)));
        }

        #[unsafe(method(cancelPreviousPerformRequestsWithTarget:))]
        fn cancel_all(target: &AnyObject) {
            cancel(target, None);
        }

        #[unsafe(method(performSelectorOnMainThread:withObject:waitUntilDone:))]
        fn perform_on_main(&self, selector: Sel, argument: Option<&AnyObject>, wait: bool) {
            let message = Request::new(this(self), selector, argument);
            perform_on(core::main_shared(), message, wait, vec![Mode::COMMON]);
        }

        #[unsafe(method(performSelectorOnMainThread:withObject:waitUntilDone:modes:))]
        fn perform_on_main_in_modes(&self, selector: Sel, argument: Option<&AnyObject>, wait: bool, modes: Option<&AnyObject>) {
            let message = Request::new(this(self), selector, argument);
            perform_on(core::main_shared(), message, wait, modes_or_common(modes));
        }

        #[unsafe(method(performSelector:onThread:withObject:waitUntilDone:))]
        fn perform_on_thread(&self, selector: Sel, thread: &AnyObject, argument: Option<&AnyObject>, wait: bool) {
            on_thread(this(self), selector, thread, argument, wait, vec![Mode::COMMON]);
        }

        #[unsafe(method(performSelector:onThread:withObject:waitUntilDone:modes:))]
        fn perform_on_thread_in_modes(
            &self,
            selector: Sel,
            thread: &AnyObject,
            argument: Option<&AnyObject>,
            wait: bool,
            modes: Option<&AnyObject>,
        ) {
            on_thread(this(self), selector, thread, argument, wait, modes_or_common(modes));
        }

        #[unsafe(method(performSelectorInBackground:withObject:))]
        fn perform_in_background(&self, selector: Sel, argument: Option<&AnyObject>) {
            crate::thread::detach(this(self), selector, argument);
        }
    }
);

/// The receiver of a method copied onto `NSObject`.
fn this(helper: &Performing) -> &AnyObject {
    // SAFETY: these methods run with any object as the receiver.
    unsafe { &*(helper as *const Performing).cast::<AnyObject>() }
}

fn on_thread(
    target: &AnyObject,
    selector: Sel,
    thread: &AnyObject,
    argument: Option<&AnyObject>,
    wait: bool,
    modes: Vec<Mode>,
) {
    let Some(thread) = thread_impl(thread) else { return };
    perform_on(&thread.run_loop(), Request::new(target, selector, argument), wait, modes);
}

/// Copy the methods onto `NSObject`. Idempotent.
pub(crate) fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let helper = Performing::class();
        let target = NSObject::class();
        let instance = [
            sel!(performSelector:withObject:afterDelay:),
            sel!(performSelector:withObject:afterDelay:inModes:),
            sel!(performSelectorOnMainThread:withObject:waitUntilDone:),
            sel!(performSelectorOnMainThread:withObject:waitUntilDone:modes:),
            sel!(performSelector:onThread:withObject:waitUntilDone:),
            sel!(performSelector:onThread:withObject:waitUntilDone:modes:),
            sel!(performSelectorInBackground:withObject:),
        ];
        for sel in instance {
            copy_method(helper.instance_method(sel).expect("helper method"), target, sel);
        }
        let class = [
            sel!(cancelPreviousPerformRequestsWithTarget:selector:object:),
            sel!(cancelPreviousPerformRequestsWithTarget:),
        ];
        for sel in class {
            copy_method(helper.class_method(sel).expect("helper method"), target.metaclass(), sel);
        }
    });
}

fn copy_method(method: &objc2::runtime::Method, target: &AnyClass, sel: Sel) {
    // SAFETY: the implementation treats its receiver as any object, which
    // is what every object on the target class is.
    unsafe {
        objc2::ffi::class_addMethod(
            (target as *const AnyClass).cast_mut(),
            sel,
            method.implementation(),
            objc2::ffi::method_getTypeEncoding(method),
        );
    }
}
