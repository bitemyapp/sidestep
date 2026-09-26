//! Run-loop observers: callouts at points of each run (entry, before
//! timers, before sources, before and after sleeping, exit).
//!
//! An observer is either a `CFRunLoopObserver` object (made through the
//! CoreFoundation functions, with a block or a callback) or a Rust closure
//! registered through [`super::RunLoop::add_observer`]. Closures stay on
//! their loop's thread and may capture anything; they are `Fn`, so a
//! closure can be re-entered by a nested run it starts.
//!
//! Each loop keeps its observers in one list sorted by (order, sequence):
//! ascending order, and registration order among equal orders, as macOS
//! calls them (see `conformance/tests/runloop.rs`).

use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::{CFRunLoopActivity, CFRunLoopObserver};

use super::Activity;
use super::core::{Dead, Shared, State, on_owner, with_state};
use super::modes::{Mode, Registration};
use crate::thread::lock;

pub(crate) type ObserverBlock = RcBlock<dyn Fn(*mut CFRunLoopObserver, CFRunLoopActivity)>;

/// A CoreFoundation observer callback and its context.
pub(crate) struct Callback {
    pub(crate) callout: unsafe extern "C-unwind" fn(*mut CFRunLoopObserver, CFRunLoopActivity, *mut c_void),
    pub(crate) info: *mut c_void,
    pub(crate) release: Option<unsafe extern "C-unwind" fn(*const c_void)>,
}

impl Drop for Callback {
    fn drop(&mut self) {
        if let Some(release) = self.release {
            // SAFETY: the context's own release function, called once.
            unsafe { release(self.info) };
        }
    }
}

pub(crate) enum Callout {
    Block(ObserverBlock),
    Callback(Callback),
}

pub(crate) struct ObserverSched {
    owner: Option<Arc<Shared>>,
    reg: Registration,
    /// First registration, for ordering among equal orders.
    seq: Option<u64>,
}

pub(crate) struct ObserverIvars {
    pub(crate) activities: usize,
    pub(crate) repeats: bool,
    pub(crate) order: isize,
    valid: AtomicBool,
    callout: Mutex<Option<Callout>>,
    sched: Mutex<ObserverSched>,
}

impl ObserverIvars {
    pub(crate) fn new(activities: usize, repeats: bool, order: isize, callout: Callout) -> Self {
        ObserverIvars {
            activities,
            repeats,
            order,
            valid: AtomicBool::new(true),
            callout: Mutex::new(Some(callout)),
            sched: Mutex::new(ObserverSched { owner: None, reg: Registration::default(), seq: None }),
        }
    }
}

// A `CFRunLoopObserver`. On macOS these are CoreFoundation objects; here,
// a private class.
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepRunLoopObserver"]
    #[ivars = ObserverIvars]
    pub(crate) struct ObserverObject;
);

impl ObserverObject {
    pub(crate) fn new(ivars: ObserverIvars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.ivars().valid.load(Ordering::Acquire)
    }

    pub(crate) fn as_cf(&self) -> *mut CFRunLoopObserver {
        (self as *const Self).cast_mut().cast()
    }

    /// The object behind a `CFRunLoopObserverRef`.
    ///
    /// # Safety
    /// `ptr` must be an observer this module made.
    pub(crate) unsafe fn from_cf<'a>(ptr: *const CFRunLoopObserver) -> &'a ObserverObject {
        // SAFETY: guaranteed by the caller.
        unsafe { &*ptr.cast::<ObserverObject>() }
    }

    fn call(&self, activity: Activity) {
        enum Now {
            Block(ObserverBlock),
            Callback(unsafe extern "C-unwind" fn(*mut CFRunLoopObserver, CFRunLoopActivity, *mut c_void), *mut c_void),
        }
        let now = match &*lock(&self.ivars().callout) {
            None => return,
            Some(Callout::Block(b)) => Now::Block(b.clone()),
            Some(Callout::Callback(c)) => Now::Callback(c.callout, c.info),
        };
        let activity = CFRunLoopActivity(activity.0);
        match now {
            Now::Block(block) => block.call((self.as_cf(), activity)),
            // SAFETY: the callback belongs to this observer and its context.
            Now::Callback(callout, info) => unsafe { callout(self.as_cf(), activity, info) },
        }
        if !self.ivars().repeats {
            invalidate(self);
        }
    }
}

/// An observer as a loop's list holds it.
#[derive(Clone)]
pub(crate) enum ObserverTarget {
    Object(Retained<ObserverObject>),
    Closure { id: u64, f: Rc<dyn Fn(Activity)>, valid: Rc<Cell<bool>> },
}

impl ObserverTarget {
    pub(crate) fn call(&self, activity: Activity) {
        match self {
            ObserverTarget::Object(o) => {
                if o.is_valid() {
                    o.call(activity);
                }
            }
            ObserverTarget::Closure { f, valid, .. } => {
                if valid.get() {
                    f(activity);
                }
            }
        }
    }
}

pub(crate) struct ObserverEntry {
    pub(crate) order: isize,
    pub(crate) seq: u64,
    pub(crate) activities: usize,
    pub(crate) reg: Registration,
    pub(crate) target: ObserverTarget,
}

impl ObserverEntry {
    /// Copy the entry's registration back to its object, for questions
    /// from other threads.
    pub(crate) fn mirror_registration(&self) {
        if let ObserverTarget::Object(o) = &self.target {
            lock(&o.ivars().sched).reg = self.reg.clone();
        }
    }
}

struct SendObserver(Retained<ObserverObject>);

// SAFETY: an observer's cross-thread state is in atomics and locks, and
// Objective-C reference counting is thread-safe.
unsafe impl Send for SendObserver {}

impl State {
    fn insert_observer(&mut self, entry: ObserverEntry) {
        let at = self.observers.partition_point(|e| (e.order, e.seq) <= (entry.order, entry.seq));
        self.observers.insert(at, entry);
    }

    fn resync_observer(&mut self, observer: &Retained<ObserverObject>) {
        if let Some(at) = self
            .observers
            .iter()
            .position(|e| matches!(&e.target, ObserverTarget::Object(o) if std::ptr::eq(&**o, &**observer)))
        {
            let entry = self.observers.remove(at);
            self.dead.push(Dead::Observer(entry.target));
        }
        let mut sched = lock(&observer.ivars().sched);
        let mine = sched.owner.as_ref().is_some_and(|o| Arc::ptr_eq(o, &self.shared));
        if !mine || !observer.is_valid() {
            return;
        }
        if sched.reg.modes.is_empty() {
            sched.owner = None;
            return;
        }
        self.known.extend(&sched.reg.modes);
        let seq = match sched.seq {
            Some(seq) => seq,
            None => *sched.seq.insert(self.next_seq()),
        };
        let ivars = observer.ivars();
        let entry = ObserverEntry {
            order: ivars.order,
            seq,
            activities: ivars.activities,
            reg: sched.reg.clone(),
            target: ObserverTarget::Object(observer.clone()),
        };
        drop(sched);
        self.insert_observer(entry);
    }

    /// Register a closure observer on this (the owner's) loop.
    pub(crate) fn add_closure_observer(
        &mut self,
        modes: &[Mode],
        activities: Activity,
        order: isize,
        f: Rc<dyn Fn(Activity)>,
    ) -> u64 {
        let mut reg = Registration::default();
        for &mode in modes {
            reg.add(mode, &self.common);
        }
        self.known.extend(&reg.modes);
        let seq = self.next_seq();
        let target = ObserverTarget::Closure { id: seq, f, valid: Rc::new(Cell::new(true)) };
        self.insert_observer(ObserverEntry { order, seq, activities: activities.0, reg, target });
        seq
    }

    pub(crate) fn remove_closure_observer(&mut self, id: u64) {
        if let Some(at) =
            self.observers.iter().position(|e| matches!(&e.target, ObserverTarget::Closure { id: i, .. } if *i == id))
        {
            let entry = self.observers.remove(at);
            if let ObserverTarget::Closure { valid, .. } = &entry.target {
                valid.set(false);
            }
            self.dead.push(Dead::Observer(entry.target));
        }
    }
}

fn resync(shared: &Arc<Shared>, observer: &ObserverObject) {
    let observer = SendObserver(observer.retain());
    on_owner(shared, move || {
        let observer = observer;
        with_state(|s| s.resync_observer(&observer.0));
    });
}

/// Add an observer object to `mode` of `shared`. An observer belongs to
/// one loop at a time; an invalid one is ignored.
pub(crate) fn add(shared: &Arc<Shared>, observer: &ObserverObject, mode: Mode) {
    if !observer.is_valid() {
        return;
    }
    {
        let mut sched = lock(&observer.ivars().sched);
        if sched.owner.as_ref().is_some_and(|o| !Arc::ptr_eq(o, shared)) {
            eprintln!("sidestep: a run-loop observer can be added to only one run loop at a time");
            return;
        }
        sched.owner = Some(shared.clone());
        let common = lock(&shared.common).clone();
        sched.reg.add(mode, &common);
    }
    resync(shared, observer);
}

pub(crate) fn remove(shared: &Arc<Shared>, observer: &ObserverObject, mode: Mode) {
    {
        let mut sched = lock(&observer.ivars().sched);
        if !sched.owner.as_ref().is_some_and(|o| Arc::ptr_eq(o, shared)) {
            return;
        }
        let common = lock(&shared.common).clone();
        sched.reg.remove(mode, &common);
    }
    resync(shared, observer);
}

pub(crate) fn contains(shared: &Arc<Shared>, observer: &ObserverObject, mode: Mode) -> bool {
    let sched = lock(&observer.ivars().sched);
    sched.owner.as_ref().is_some_and(|o| Arc::ptr_eq(o, shared)) && sched.reg.contains(mode)
}

/// Stop the observer for good; it leaves its loop and lets go of its
/// callout.
pub(crate) fn invalidate(observer: &ObserverObject) {
    if !observer.ivars().valid.swap(false, Ordering::AcqRel) {
        return;
    }
    let callout = lock(&observer.ivars().callout).take();
    let owner = {
        let mut sched = lock(&observer.ivars().sched);
        sched.reg = Registration::default();
        sched.owner.take()
    };
    if let Some(owner) = owner {
        resync(&owner, observer);
    }
    drop(callout);
}

/// Load the observer class. It has no shell, so its type defines it.
pub(crate) fn load() {
    let _ = ObserverObject::class();
}
