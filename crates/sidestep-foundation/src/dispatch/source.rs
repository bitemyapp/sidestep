//! Dispatch sources: data merged by the program (add, or, replace),
//! timers, and file-system events (vnode) through inotify.
//!
//! A source starts inactive; resuming or activating it the first time
//! runs its registration handler and starts its timer or watch. Events
//! merge into pending data and queue one delivery on the target queue;
//! the event handler sees the merged data through
//! `dispatch_source_get_data`, and a source's handler never runs twice at
//! once. Cancelling stops the events and queues the cancel handler.
//!
//! Vnode sources watch the path their descriptor was opened with, found
//! through `/proc/self/fd`. One thread reads every watch's inotify events
//! and maps them back to libdispatch's flags: a write or change of size is
//! `WRITE` and `EXTEND`, changes inside a watched directory are `WRITE`, a
//! rename of the file itself is `RENAME`, and its deletion is `DELETE`
//! (reported when its link count drops to zero, since an open descriptor
//! keeps the file itself alive). Other source types aren't supported:
//! creating one returns null.

use std::collections::HashMap;
use std::ffi::{CString, c_void};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use block2::{DynBlock, RcBlock};

use super::after::{self, Due};
use super::object::{self, Function, Kind, body};
use super::queue::{Obj, enqueue, function};
use crate::runloop::core::Work;
use crate::thread::lock;

/// `dispatch_source_type_s`: only its address matters.
#[repr(C)]
pub struct SourceType {
    kind: SourceKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum SourceKind {
    DataAdd,
    DataOr,
    DataReplace,
    Timer,
    Vnode,
    Unsupported,
}

macro_rules! source_types {
    ($($name:ident = $kind:ident;)*) => {$(
        #[unsafe(no_mangle)]
        pub static $name: SourceType = SourceType { kind: SourceKind::$kind };
    )*};
}

source_types! {
    _dispatch_source_type_data_add = DataAdd;
    _dispatch_source_type_data_or = DataOr;
    _dispatch_source_type_data_replace = DataReplace;
    _dispatch_source_type_timer = Timer;
    _dispatch_source_type_vnode = Vnode;
    _dispatch_source_type_read = Unsupported;
    _dispatch_source_type_write = Unsupported;
    _dispatch_source_type_signal = Unsupported;
    _dispatch_source_type_proc = Unsupported;
    _dispatch_source_type_mach_send = Unsupported;
    _dispatch_source_type_mach_recv = Unsupported;
    _dispatch_source_type_memorypressure = Unsupported;
}

enum Handler {
    Function(Function),
    Block(RcBlock<dyn Fn()>),
}

#[derive(Default)]
struct Handlers {
    event: Option<Handler>,
    cancel: Option<Handler>,
    registration: Option<Handler>,
}

#[derive(Default)]
struct Timer {
    /// Bumped by every `dispatch_source_set_timer`, so stale deadlines are
    /// ignored.
    generation: u64,
    next: Option<Instant>,
    interval: Option<Duration>,
}

pub(crate) struct Source {
    kind: SourceKind,
    handle: usize,
    mask: usize,
    queue: Mutex<Obj>,
    handlers: Mutex<Handlers>,
    /// Data merged since the last delivery.
    pending: Mutex<usize>,
    /// Data the running event handler sees.
    data: AtomicUsize,
    scheduled: AtomicBool,
    suspended: AtomicUsize,
    activated: AtomicBool,
    cancelled: AtomicBool,
    timer: Mutex<Timer>,
    /// The inotify watch of a vnode source.
    watch: Mutex<Option<i32>>,
}

// SAFETY: handlers and data are behind locks and atomics; handlers run on
// the source's queue, as libdispatch's contract says.
unsafe impl Send for Source {}
unsafe impl Sync for Source {}

/// # Safety
/// `source` must be a live dispatch source.
unsafe fn source<'a>(source: *const c_void) -> &'a Source {
    // SAFETY: guaranteed by the caller.
    match unsafe { &body(source).kind } {
        Kind::Source(source) => source,
        _ => panic!("sidestep: a dispatch source function was passed an object that isn't a source"),
    }
}

pub(crate) fn is_source(object: *const c_void) -> bool {
    // SAFETY: callers pass live dispatch objects.
    matches!(unsafe { &body(object).kind }, Kind::Source(_))
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_create(
    kind: *const SourceType,
    handle: usize,
    mask: usize,
    queue: *mut c_void,
) -> *mut c_void {
    // SAFETY: the caller passes one of the source types.
    let kind = unsafe { &*kind }.kind;
    if kind == SourceKind::Unsupported {
        return std::ptr::null_mut();
    }
    let queue = if queue.is_null() { super::dispatch_get_global_queue(0, 0) } else { queue };
    object::create(Kind::Source(Source {
        kind,
        handle,
        mask,
        queue: Mutex::new(Obj::retain(queue)),
        handlers: Mutex::new(Handlers::default()),
        pending: Mutex::new(0),
        data: AtomicUsize::new(0),
        scheduled: AtomicBool::new(false),
        suspended: AtomicUsize::new(1),
        activated: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        timer: Mutex::new(Timer::default()),
        watch: Mutex::new(None),
    }))
}

fn set_handler(s: *mut c_void, which: fn(&mut Handlers) -> &mut Option<Handler>, handler: Option<Handler>) {
    // SAFETY: the caller passes a source.
    let old = std::mem::replace(which(&mut lock(&unsafe { source(s) }.handlers)), handler);
    drop(old);
}

fn block_handler(block: *const DynBlock<dyn Fn()>) -> Option<Handler> {
    // SAFETY: the caller passes a block or null.
    unsafe { block.as_ref() }.map(|b| Handler::Block(b.copy()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_set_event_handler(s: *mut c_void, block: *const DynBlock<dyn Fn()>) {
    set_handler(s, |h| &mut h.event, block_handler(block));
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_set_event_handler_f(
    s: *mut c_void,
    f: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    set_handler(s, |h| &mut h.event, f.map(|f| Handler::Function(function(f))));
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_set_cancel_handler(s: *mut c_void, block: *const DynBlock<dyn Fn()>) {
    set_handler(s, |h| &mut h.cancel, block_handler(block));
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_set_cancel_handler_f(
    s: *mut c_void,
    f: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    set_handler(s, |h| &mut h.cancel, f.map(|f| Handler::Function(function(f))));
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_set_registration_handler(
    s: *mut c_void,
    block: *const DynBlock<dyn Fn()>,
) {
    set_handler(s, |h| &mut h.registration, block_handler(block));
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_set_registration_handler_f(
    s: *mut c_void,
    f: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    set_handler(s, |h| &mut h.registration, f.map(|f| Handler::Function(function(f))));
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_get_handle(s: *mut c_void) -> usize {
    // SAFETY: the caller passes a source.
    unsafe { source(s) }.handle
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_get_mask(s: *mut c_void) -> usize {
    // SAFETY: the caller passes a source.
    unsafe { source(s) }.mask
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_get_data(s: *mut c_void) -> usize {
    // SAFETY: the caller passes a source.
    unsafe { source(s) }.data.load(Ordering::Acquire)
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_merge_data(s: *mut c_void, value: usize) {
    // SAFETY: the caller passes a source.
    let src = unsafe { source(s) };
    if matches!(src.kind, SourceKind::DataAdd | SourceKind::DataOr | SourceKind::DataReplace) {
        signal(s, value);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_testcancel(s: *mut c_void) -> isize {
    // SAFETY: the caller passes a source.
    unsafe { source(s) }.cancelled.load(Ordering::Acquire) as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_cancel(s: *mut c_void) {
    // SAFETY: the caller passes a source.
    let src = unsafe { source(s) };
    if src.cancelled.swap(true, Ordering::AcqRel) {
        return;
    }
    lock(&src.timer).generation += 1;
    if let Some(wd) = lock(&src.watch).take() {
        unwatch(wd, s);
    }
    let handler = lock(&src.handlers).cancel.take();
    if let Some(handler) = handler {
        queue_handler(s, src, handler);
    }
}

/// Queue a handler (with the source's context) on the source's queue.
fn queue_handler(s: *mut c_void, src: &Source, handler: Handler) {
    struct Call {
        source: Obj,
        handler: Handler,
    }
    unsafe extern "C-unwind" fn run(ctx: *mut c_void) {
        // SAFETY: made from a Box below; runs once.
        let call = unsafe { Box::from_raw(ctx.cast::<Call>()) };
        call_handler(call.source.ptr(), &call.handler);
    }
    let call = Box::new(Call { source: Obj::retain(s), handler });
    let queue = Obj::retain(lock(&src.queue).ptr());
    enqueue(queue.ptr(), Work { f: run, ctx: Box::into_raw(call).cast() }, false);
}

fn call_handler(s: *mut c_void, handler: &Handler) {
    match handler {
        // SAFETY: function handlers take the source's context.
        Handler::Function(f) => unsafe { f(object::context(s)) },
        Handler::Block(block) => block.call(()),
    }
}

/// Merge an event's data and queue a delivery if none is queued.
fn signal(s: *mut c_void, value: usize) {
    // SAFETY: callers pass live sources.
    let src = unsafe { source(s) };
    if src.cancelled.load(Ordering::Acquire) {
        return;
    }
    {
        let mut pending = lock(&src.pending);
        *pending = match src.kind {
            SourceKind::DataAdd | SourceKind::Timer => pending.wrapping_add(value),
            SourceKind::DataReplace => value,
            SourceKind::DataOr | SourceKind::Vnode | SourceKind::Unsupported => *pending | value,
        };
        if *pending == 0 {
            return;
        }
    }
    schedule(s, src);
}

fn schedule(s: *mut c_void, src: &Source) {
    if src.suspended.load(Ordering::Acquire) > 0 || src.cancelled.load(Ordering::Acquire) {
        return;
    }
    if src.scheduled.swap(true, Ordering::AcqRel) {
        return;
    }
    unsafe extern "C-unwind" fn deliver(ctx: *mut c_void) {
        // SAFETY: the delivery owns one reference to the source.
        let object = unsafe { Obj::from_raw(ctx) };
        let s = object.ptr();
        // SAFETY: a live source.
        let src = unsafe { source(s) };
        let data = std::mem::take(&mut *lock(&src.pending));
        if data != 0 && !src.cancelled.load(Ordering::Acquire) {
            src.data.store(data, Ordering::Release);
            let handler = match &lock(&src.handlers).event {
                None => None,
                Some(Handler::Function(f)) => Some(Handler::Function(*f)),
                Some(Handler::Block(b)) => Some(Handler::Block(b.clone())),
            };
            if let Some(handler) = handler {
                call_handler(s, &handler);
            }
        }
        src.scheduled.store(false, Ordering::Release);
        if *lock(&src.pending) != 0 {
            schedule(s, src);
        }
    }
    let queue = Obj::retain(lock(&src.queue).ptr());
    enqueue(queue.ptr(), Work { f: deliver, ctx: Obj::retain(s).into_raw() }, false);
}

pub(crate) fn suspend(s: *mut c_void) {
    // SAFETY: callers pass live sources.
    unsafe { source(s) }.suspended.fetch_add(1, Ordering::AcqRel);
}

pub(crate) fn resume(s: *mut c_void) {
    // SAFETY: callers pass live sources.
    let src = unsafe { source(s) };
    let was = src.suspended.fetch_sub(1, Ordering::AcqRel);
    assert!(was > 0, "sidestep: dispatch_resume: over-resumed source");
    if was != 1 {
        return;
    }
    if !src.activated.swap(true, Ordering::AcqRel) {
        register(s, src);
    }
    if *lock(&src.pending) != 0 {
        schedule(s, src);
    }
}

pub(crate) fn activate(s: *mut c_void) {
    // SAFETY: callers pass live sources.
    if !unsafe { source(s) }.activated.load(Ordering::Acquire) {
        resume(s);
    }
}

/// First activation: the registration handler, then the timer or watch.
fn register(s: *mut c_void, src: &Source) {
    let handler = lock(&src.handlers).registration.take();
    if let Some(handler) = handler {
        queue_handler(s, src, handler);
    }
    match src.kind {
        SourceKind::Timer => arm(s, src),
        SourceKind::Vnode => watch(s, src),
        _ => {}
    }
}

// Timers.

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_source_set_timer(s: *mut c_void, start: u64, interval: u64, _leeway: u64) {
    // SAFETY: the caller passes a source.
    let src = unsafe { source(s) };
    if src.kind != SourceKind::Timer {
        return;
    }
    {
        let mut timer = lock(&src.timer);
        timer.generation += 1;
        timer.next = super::time::deadline(start);
        timer.interval = match interval {
            0 | u64::MAX => None,
            nanos => Some(Duration::from_nanos(nanos)),
        };
    }
    // Setting a timer again drops what the old one had counted.
    *lock(&src.pending) = 0;
    if src.activated.load(Ordering::Acquire) {
        arm(s, src);
    }
}

fn arm(s: *mut c_void, src: &Source) {
    let timer = lock(&src.timer);
    if let Some(next) = timer.next
        && !src.cancelled.load(Ordering::Acquire)
    {
        after::at(next, Due::Source(Obj::retain(s), timer.generation));
    }
}

/// A timer source's deadline came: count the fire (and any missed ones)
/// and arm the next.
pub(crate) fn timer_fired(source_obj: Obj, generation: u64) {
    let s = source_obj.ptr();
    // SAFETY: a live source.
    let src = unsafe { source(s) };
    let fires = {
        let mut timer = lock(&src.timer);
        if timer.generation != generation || src.cancelled.load(Ordering::Acquire) {
            return;
        }
        match (timer.next, timer.interval) {
            (Some(due), Some(interval)) => {
                let behind = Instant::now().saturating_duration_since(due);
                let missed = (behind.as_nanos() / interval.as_nanos().max(1)) as u32;
                timer.next = due.checked_add(interval * (missed + 1));
                1 + missed as usize
            }
            _ => {
                timer.next = None;
                1
            }
        }
    };
    signal(s, fires);
    arm(s, src);
}

// Vnode sources.

const VNODE_DELETE: usize = 0x1;
const VNODE_WRITE: usize = 0x2;
const VNODE_EXTEND: usize = 0x4;
const VNODE_ATTRIB: usize = 0x8;
const VNODE_LINK: usize = 0x10;
const VNODE_RENAME: usize = 0x20;
const VNODE_REVOKE: usize = 0x40;

struct Watches {
    fd: i32,
    by_watch: HashMap<i32, Vec<Obj>>,
}

static WATCHES: LazyLock<Mutex<Option<Watches>>> = LazyLock::new(|| Mutex::new(None));

fn inotify_mask(mask: usize) -> u32 {
    let mut m = 0;
    if mask & VNODE_WRITE != 0 {
        m |= libc::IN_MODIFY | libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO;
    }
    if mask & VNODE_EXTEND != 0 {
        m |= libc::IN_MODIFY;
    }
    if mask & (VNODE_ATTRIB | VNODE_LINK | VNODE_DELETE) != 0 {
        m |= libc::IN_ATTRIB;
    }
    if mask & VNODE_DELETE != 0 {
        m |= libc::IN_DELETE_SELF;
    }
    if mask & VNODE_RENAME != 0 {
        m |= libc::IN_MOVE_SELF;
    }
    if mask & VNODE_REVOKE != 0 {
        m |= libc::IN_UNMOUNT;
    }
    m
}

/// The flags an inotify event means for a source watching `fd`.
fn vnode_flags(event: u32, fd: i32) -> usize {
    let mut flags = 0;
    if event & libc::IN_MODIFY != 0 {
        flags |= VNODE_WRITE | VNODE_EXTEND;
    }
    if event & (libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO) != 0 {
        flags |= VNODE_WRITE;
    }
    if event & libc::IN_ATTRIB != 0 {
        flags |= VNODE_ATTRIB | VNODE_LINK;
        // SAFETY: fstat writes into a zeroed stat buffer.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut stat) } == 0 && stat.st_nlink == 0 {
            flags |= VNODE_DELETE;
        }
    }
    if event & libc::IN_DELETE_SELF != 0 {
        flags |= VNODE_DELETE;
    }
    if event & libc::IN_MOVE_SELF != 0 {
        flags |= VNODE_RENAME;
    }
    if event & libc::IN_UNMOUNT != 0 {
        flags |= VNODE_REVOKE;
    }
    flags
}

fn watch(s: *mut c_void, src: &Source) {
    let Ok(path) = std::fs::read_link(format!("/proc/self/fd/{}", src.handle)) else { return };
    let Ok(path) = CString::new(path.into_os_string().into_encoded_bytes()) else { return };
    let mut watches = lock(&WATCHES);
    if watches.is_none() {
        // SAFETY: plain syscall.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if fd < 0 {
            return;
        }
        *watches = Some(Watches { fd, by_watch: HashMap::new() });
        std::thread::Builder::new()
            .name("dispatch-vnode".into())
            .spawn(move || read_events(fd))
            .expect("sidestep: couldn't start a thread");
    }
    let watches = watches.as_mut().expect("just made");
    // SAFETY: a valid descriptor and NUL-terminated path.
    let wd = unsafe { libc::inotify_add_watch(watches.fd, path.as_ptr(), inotify_mask(src.mask) | libc::IN_MASK_ADD) };
    if wd < 0 {
        return;
    }
    watches.by_watch.entry(wd).or_default().push(Obj::retain(s));
    *lock(&src.watch) = Some(wd);
}

fn unwatch(wd: i32, s: *mut c_void) {
    let removed = {
        let mut watches = lock(&WATCHES);
        let Some(watches) = watches.as_mut() else { return };
        let mut removed = Vec::new();
        if let Some(list) = watches.by_watch.get_mut(&wd) {
            if let Some(at) = list.iter().position(|o| o.ptr() == s) {
                removed.push(list.remove(at));
            }
            if list.is_empty() {
                watches.by_watch.remove(&wd);
                // SAFETY: a watch we added.
                unsafe { libc::inotify_rm_watch(watches.fd, wd) };
            }
        }
        removed
    };
    drop(removed);
}

fn read_events(fd: i32) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        // SAFETY: reading into our buffer.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        let mut at = 0;
        while at + size_of::<libc::inotify_event>() <= n as usize {
            // SAFETY: the kernel wrote whole events; read unaligned.
            let event = unsafe { buf.as_ptr().add(at).cast::<libc::inotify_event>().read_unaligned() };
            at += size_of::<libc::inotify_event>() + event.len as usize;
            let targets: Vec<Obj> = {
                let watches = lock(&WATCHES);
                watches
                    .as_ref()
                    .and_then(|w| w.by_watch.get(&event.wd))
                    .map_or_else(Vec::new, |list| list.iter().map(|o| Obj::retain(o.ptr())).collect())
            };
            for target in targets {
                // SAFETY: a live source.
                let src = unsafe { source(target.ptr()) };
                let flags = vnode_flags(event.mask, src.handle as i32) & src.mask;
                if flags != 0 {
                    signal(target.ptr(), flags);
                }
            }
        }
    }
}
