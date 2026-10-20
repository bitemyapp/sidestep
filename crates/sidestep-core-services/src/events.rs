//! FSEvents streams. `notify` supplies recursive inotify watches; a collector
//! coalesces paths off the caller's thread, then delivers on the client's
//! dispatch queue or run loop. A generation cancels queued deliveries on stop.

use std::collections::BTreeMap;
use std::ffi::{CString, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use objc2::rc::autoreleasepool;
use objc2_core_foundation::{CFArray, CFRetained, CFRunLoop, CFString};

const USE_CF: u32 = 1;
const NO_DEFER: u32 = 2;
const WATCH_ROOT: u32 = 4;
const FILE_EVENTS: u32 = 16;
const MUST_SCAN: u32 = 1;
const USER_DROPPED: u32 = 2;
const HISTORY_DONE: u32 = 16;
const ROOT_CHANGED: u32 = 32;
const CREATED: u32 = 0x100;
const REMOVED: u32 = 0x200;
const INODE_META: u32 = 0x400;
const RENAMED: u32 = 0x800;
const MODIFIED: u32 = 0x1000;
const IS_FILE: u32 = 0x10000;
const IS_DIR: u32 = 0x20000;
const IS_SYMLINK: u32 = 0x40000;
static EVENT_ID: AtomicU64 = AtomicU64::new(1);

type Callback = unsafe extern "C-unwind" fn(*const Stream, *mut c_void, usize, *mut c_void, *const u32, *const u64);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Context {
    version: isize,
    info: *mut c_void,
    retain: Option<unsafe extern "C-unwind" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C-unwind" fn(*const c_void)>,
    description: Option<unsafe extern "C-unwind" fn(*const c_void) -> *const CFString>,
}

struct Client {
    info: usize,
    release: Option<unsafe extern "C-unwind" fn(*const c_void)>,
    callback: Callback,
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Some(release) = self.release {
            // The context's retain callback transferred this reference to us.
            unsafe { release(self.info as *const c_void) };
        }
    }
}

#[derive(Clone)]
struct RunTarget {
    run_loop: CFRetained<CFRunLoop>,
    mode: CFRetained<CFString>,
}
// Only CFRunLoopPerformBlock and CFRunLoopWakeUp touch the loop from other
// threads; both operations are thread-safe. Mode strings are immutable.
unsafe impl Send for RunTarget {}
unsafe impl Sync for RunTarget {}

#[derive(Clone, Default)]
enum Target {
    #[default]
    None,
    Queue(DispatchRetained<DispatchQueue>),
    RunLoops(Vec<RunTarget>),
}
impl Target {
    fn schedule(&self, job: impl FnOnce() + Send + 'static) {
        match self {
            Self::None => {}
            Self::Queue(queue) => queue.exec_async(job),
            Self::RunLoops(loops) => {
                // A stream scheduled on several loops delivers each batch once,
                // on the first loop to service it.
                let job = Arc::new(Mutex::new(Some(job)));
                for target in loops {
                    let job = job.clone();
                    let block = RcBlock::new(move || {
                        let work = job.lock().unwrap().take();
                        if let Some(work) = work {
                            work();
                        }
                    });
                    unsafe { target.run_loop.perform_block(Some(target.mode.as_ref()), Some(&block)) };
                    target.run_loop.wake_up();
                }
            }
        }
    }
    fn is_set(&self) -> bool {
        !matches!(self, Self::None) && !matches!(self, Self::RunLoops(loops) if loops.is_empty())
    }
}

struct Running {
    watcher: RecommendedWatcher,
    tx: mpsc::Sender<Command>,
    worker: JoinHandle<()>,
}
impl Running {
    fn stop(self) {
        // No worker ever waits for a callback, so stopping from within the
        // stream's own callback cannot deadlock with a dispatch queue.
        drop(self.watcher);
        let _ = self.tx.send(Command::Stop);
        if self.worker.thread().id() != std::thread::current().id() {
            let _ = self.worker.join();
        }
    }
}

#[derive(Default)]
struct State {
    target: Target,
    generation: u64,
    invalid: bool,
    running: Option<Running>,
    exclusions: Vec<PathBuf>,
}

pub struct Stream {
    paths: Vec<PathBuf>,
    roots: Vec<PathBuf>,
    flags: u32,
    since: u64,
    latency: Duration,
    client: Client,
    latest: AtomicU64,
    issued: AtomicU64,
    state: Mutex<State>,
    callout: Mutex<()>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(running) = self.state.get_mut().unwrap().running.take() {
            running.stop();
        }
    }
}

enum Command {
    Event(notify::Result<Event>),
    Flush { complete: Option<mpsc::Sender<()>>, scheduled: Option<mpsc::Sender<u64>> },
    Stop,
}
type Pending = BTreeMap<PathBuf, u32>;

// The C API owns Arc references, independently of the collector and queued
// callbacks. An exported call always starts with a live caller-owned reference.
unsafe fn retain(stream: *const Stream) -> Arc<Stream> {
    unsafe {
        Arc::increment_strong_count(stream);
        Arc::from_raw(stream)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamCreate(
    _allocator: *const c_void,
    callback: Option<Callback>,
    context: *const Context,
    paths: *const CFArray<CFString>,
    since: u64,
    latency: f64,
    flags: u32,
) -> *const Stream {
    let (Some(callback), Some(paths)) = (callback, unsafe { paths.as_ref() }) else { return std::ptr::null() };
    // Linux inotify has no writer PID or persistent journal. Never silently
    // accept options whose required semantics the backend cannot supply.
    if !latency.is_finite() || latency < 0.0 || flags & !(USE_CF | NO_DEFER | WATCH_ROOT | FILE_EVENTS) != 0 {
        return std::ptr::null();
    }
    let paths: Vec<PathBuf> = paths.to_vec().iter().map(|s| PathBuf::from(s.to_string())).collect();
    if paths.is_empty() || paths.iter().any(|p| !p.is_absolute()) {
        return std::ptr::null();
    }
    let roots = paths.iter().map(|p| p.canonicalize().unwrap_or_else(|_| p.clone())).collect();
    let mut client = Client { callback, info: 0, release: None };
    if let Some(context) = unsafe { context.as_ref() } {
        if context.version != 0 {
            return std::ptr::null();
        }
        client.info = if let Some(retain) = context.retain {
            unsafe { retain(context.info) as usize }
        } else {
            context.info as usize
        };
        client.release = context.release;
    }
    Arc::into_raw(Arc::new(Stream {
        paths,
        roots,
        flags,
        since,
        latency: Duration::try_from_secs_f64(latency).unwrap_or(Duration::MAX),
        client,
        latest: AtomicU64::new(0),
        issued: AtomicU64::new(0),
        state: Mutex::new(State::default()),
        callout: Mutex::new(()),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamRetain(stream: *const Stream) {
    unsafe { Arc::increment_strong_count(stream) };
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamRelease(stream: *const Stream) {
    unsafe { Arc::decrement_strong_count(stream) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamSetDispatchQueue(stream: *const Stream, queue: *mut DispatchQueue) {
    let stream = unsafe { &*stream };
    let mut state = stream.state.lock().unwrap();
    if state.invalid || state.running.is_some() {
        return;
    }
    state.target = NonNull::new(queue).map_or(Target::None, |q| Target::Queue(unsafe { DispatchRetained::retain(q) }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamScheduleWithRunLoop(
    stream: *const Stream,
    rl: *mut CFRunLoop,
    mode: *mut CFString,
) {
    let (Some(rl), Some(mode)) = (NonNull::new(rl), NonNull::new(mode)) else { return };
    let mut state = unsafe { &*stream }.state.lock().unwrap();
    if state.invalid || state.running.is_some() {
        return;
    }
    if !matches!(state.target, Target::RunLoops(_)) {
        state.target = Target::RunLoops(Vec::new());
    }
    let Target::RunLoops(loops) = &mut state.target else { unreachable!() };
    if !loops.iter().any(|t| std::ptr::eq(&*t.run_loop, rl.as_ptr()) && std::ptr::eq(&*t.mode, mode.as_ptr())) {
        loops
            .push(RunTarget { run_loop: unsafe { CFRetained::retain(rl) }, mode: unsafe { CFRetained::retain(mode) } });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamUnscheduleFromRunLoop(
    stream: *const Stream,
    rl: *const CFRunLoop,
    mode: *const CFString,
) {
    let mut state = unsafe { &*stream }.state.lock().unwrap();
    if let Target::RunLoops(loops) = &mut state.target {
        loops.retain(|t| !(std::ptr::eq(&*t.run_loop, rl) && unsafe { mode.as_ref() }.is_some_and(|m| *t.mode == *m)));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamStart(stream: *const Stream) -> u8 {
    let stream = unsafe { retain(stream) };
    let mut state = stream.state.lock().unwrap();
    if state.invalid || !state.target.is_set() {
        return 0;
    }
    if state.running.is_some() {
        return 1;
    }
    let (tx, rx) = mpsc::channel();
    let events = tx.clone();
    let Ok(mut watcher) = notify::recommended_watcher(move |event| {
        let _ = events.send(Command::Event(event));
    }) else {
        return 0;
    };
    let mut watches = BTreeMap::new();
    for root in &stream.roots {
        watches.insert(root.clone(), RecursiveMode::Recursive);
        if stream.flags & WATCH_ROOT != 0 {
            for parent in root.ancestors().skip(1) {
                watches.entry(parent.to_path_buf()).or_insert(RecursiveMode::NonRecursive);
            }
        }
    }
    for (path, mode) in watches {
        if watcher.watch(&path, mode).is_err() {
            return 0;
        }
    }
    state.generation += 1;
    let generation = state.generation;
    let weak = Arc::downgrade(&stream);
    let Ok(worker) =
        std::thread::Builder::new().name("sidestep-fsevents".into()).spawn(move || collect(weak, generation, rx))
    else {
        return 0;
    };
    state.running = Some(Running { watcher, tx, worker });
    1
}

fn stop(stream: &Stream, invalidate: bool) {
    let running = {
        let mut state = stream.state.lock().unwrap();
        state.generation += 1;
        state.invalid |= invalidate;
        if invalidate {
            state.target = Target::None;
        }
        state.running.take()
    };
    if let Some(running) = running {
        running.stop();
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamStop(stream: *const Stream) {
    stop(unsafe { &*stream }, false);
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamInvalidate(stream: *const Stream) {
    stop(unsafe { &*stream }, true);
}

fn collect(weak: Weak<Stream>, generation: u64, rx: mpsc::Receiver<Command>) {
    let Some(stream) = weak.upgrade() else { return };
    let (latency, flags) = (stream.latency, stream.flags);
    let mut pending = Pending::new();
    if stream.since != u64::MAX {
        // There is no disk journal on Linux. A historical request must rescan
        // rather than silently missing everything before this stream started.
        for root in &stream.roots {
            pending.insert(root.clone(), MUST_SCAN | USER_DROPPED | HISTORY_DONE);
        }
    }
    drop(stream);
    let mut deadline = if pending.is_empty() { None } else { Some(Instant::now()) };
    let mut last = None::<Instant>;
    loop {
        let message = match deadline {
            Some(at) => rx.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        let Some(stream) = weak.upgrade() else { break };
        match message {
            Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Command::Event(event)) => {
                record(&stream, event, &mut pending);
                if !pending.is_empty() && deadline.is_none() {
                    let wait = if flags & NO_DEFER != 0 && last.is_none_or(|t| t.elapsed() >= latency) {
                        Duration::ZERO
                    } else {
                        latency
                    };
                    deadline = Instant::now().checked_add(wait);
                }
            }
            Ok(Command::Flush { complete, scheduled }) => {
                deliver(&stream, generation, std::mem::take(&mut pending), complete);
                if let Some(scheduled) = scheduled {
                    let _ = scheduled.send(stream.issued.load(Ordering::SeqCst));
                }
                deadline = None;
                last = Some(Instant::now());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                deliver(&stream, generation, std::mem::take(&mut pending), None);
                deadline = None;
                last = Some(Instant::now());
            }
        }
    }
}

fn record(stream: &Stream, event: notify::Result<Event>, pending: &mut Pending) {
    let Ok(event) = event else {
        for root in &stream.roots {
            *pending.entry(root.clone()).or_default() |= MUST_SCAN | USER_DROPPED;
        }
        return;
    };
    if event.need_rescan() {
        for root in &stream.roots {
            *pending.entry(root.clone()).or_default() |= MUST_SCAN | USER_DROPPED;
        }
        return;
    }
    let flags = match event.kind {
        EventKind::Create(_) => CREATED,
        EventKind::Remove(_) => REMOVED,
        EventKind::Modify(ModifyKind::Name(_)) => RENAMED,
        EventKind::Modify(ModifyKind::Metadata(_)) => INODE_META,
        EventKind::Modify(_) => MODIFIED,
        EventKind::Any | EventKind::Other => MUST_SCAN,
        EventKind::Access(_) => return,
    };
    let state = stream.state.lock().unwrap();
    for path in event.paths {
        if stream.flags & WATCH_ROOT != 0 && flags & (REMOVED | RENAMED) != 0 {
            for root in &stream.roots {
                if root.starts_with(&path) {
                    *pending.entry(root.clone()).or_default() |= ROOT_CHANGED | MUST_SCAN;
                }
            }
        }
        if !stream.roots.iter().any(|r| path.starts_with(r)) || state.exclusions.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        if stream.flags & FILE_EVENTS == 0 {
            let directory = path.parent().unwrap_or(&path).to_path_buf();
            *pending.entry(directory).or_default() |= MUST_SCAN;
            continue;
        }
        // inotify reports a new symlink as CreateKind::File. Prefer lstat's
        // identity while it exists; after removal only the event knows it.
        let kind = match file_kind(&path) {
            0 => match event.kind {
                EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder) => IS_DIR,
                EventKind::Create(CreateKind::File) | EventKind::Remove(RemoveKind::File) => IS_FILE,
                _ => 0,
            },
            kind => kind,
        };
        *pending.entry(path).or_default() |= flags | kind;
    }
}
fn file_kind(path: &Path) -> u32 {
    match path.symlink_metadata().map(|m| m.file_type()) {
        Ok(t) if t.is_symlink() => IS_SYMLINK,
        Ok(t) if t.is_dir() => IS_DIR,
        Ok(t) if t.is_file() => IS_FILE,
        _ => 0,
    }
}

fn deliver(stream: &Arc<Stream>, generation: u64, pending: Pending, done: Option<mpsc::Sender<()>>) {
    let target = {
        let state = stream.state.lock().unwrap();
        if state.generation != generation {
            return;
        }
        state.target.clone()
    };
    let (paths, flags): (Vec<_>, Vec<_>) = pending.into_iter().unzip();
    let ids: Vec<u64> =
        flags.iter().map(|f| if f & ROOT_CHANGED != 0 { 0 } else { EVENT_ID.fetch_add(1, Ordering::SeqCst) }).collect();
    if let Some(latest) = ids.iter().copied().filter(|id| *id != 0).max() {
        stream.issued.store(latest, Ordering::SeqCst);
    }
    let stream = stream.clone();
    target.schedule(move || {
        let _serial = stream.callout.lock().unwrap();
        let valid = {
            let state = stream.state.lock().unwrap();
            state.generation == generation && !state.invalid
        };
        if valid && !paths.is_empty() {
            autoreleasepool(|_| {
                if let Some(latest) = ids.iter().copied().filter(|id| *id != 0).max() {
                    stream.latest.store(latest, Ordering::SeqCst);
                }
                let invoke = |paths: *mut c_void| unsafe {
                    (stream.client.callback)(
                        Arc::as_ptr(&stream),
                        stream.client.info as *mut c_void,
                        flags.len(),
                        paths,
                        flags.as_ptr(),
                        ids.as_ptr(),
                    );
                };
                if stream.flags & USE_CF != 0 {
                    let strings: Vec<_> = paths.iter().map(|p| CFString::from_str(&p.to_string_lossy())).collect();
                    let array = CFArray::from_retained_objects(&strings);
                    invoke((&*array as *const CFArray<CFString>).cast_mut().cast());
                } else {
                    let strings: Vec<_> = paths
                        .iter()
                        .map(|p| CString::new(p.as_os_str().as_bytes()).expect("filesystem paths have no NUL"))
                        .collect();
                    let pointers: Vec<*const c_char> = strings.iter().map(|s| s.as_ptr()).collect();
                    invoke(pointers.as_ptr().cast_mut().cast());
                }
            });
        }
        if let Some(done) = done {
            let _ = done.send(());
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamFlushAsync(stream: *const Stream) -> u64 {
    let stream = unsafe { &*stream };
    let (tx, rx) = mpsc::channel();
    {
        let state = stream.state.lock().unwrap();
        let Some(running) = &state.running else { return stream.issued.load(Ordering::SeqCst) };
        if running.tx.send(Command::Flush { complete: None, scheduled: Some(tx) }).is_err() {
            return 0;
        }
    }
    // Wait only for the collector to enqueue the batch, never for its callback.
    rx.recv().unwrap_or(0)
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamFlushSync(stream: *const Stream) {
    let (tx, rx) = mpsc::channel();
    {
        let state = unsafe { &*stream }.state.lock().unwrap();
        let Some(running) = &state.running else { return };
        if running.tx.send(Command::Flush { complete: Some(tx), scheduled: None }).is_err() {
            return;
        }
    }
    let _ = rx.recv();
}
#[unsafe(no_mangle)]
pub extern "C-unwind" fn FSEventsGetCurrentEventId() -> u64 {
    EVENT_ID.load(Ordering::SeqCst).saturating_sub(1)
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamGetLatestEventId(stream: *const Stream) -> u64 {
    unsafe { &*stream }.latest.load(Ordering::SeqCst)
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamCopyPathsBeingWatched(stream: *const Stream) -> *const CFArray<CFString> {
    let paths: Vec<_> = unsafe { &*stream }.paths.iter().map(|p| CFString::from_str(&p.to_string_lossy())).collect();
    CFRetained::into_raw(CFArray::from_retained_objects(&paths)).as_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamCopyDescription(stream: *const Stream) -> *const CFString {
    let stream = unsafe { &*stream };
    CFRetained::into_raw(CFString::from_str(&format!("<FSEventStream {:?}>", stream.paths))).as_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamShow(stream: *const Stream) {
    eprintln!("FSEventStream {:?}", unsafe { &*stream }.paths);
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn FSEventStreamSetExclusionPaths(
    stream: *const Stream,
    paths: *const CFArray<CFString>,
) -> u8 {
    let Some(paths) = (unsafe { paths.as_ref() }) else { return 0 };
    let paths: Vec<PathBuf> = paths.to_vec().iter().map(|p| PathBuf::from(p.to_string())).collect();
    if paths.len() > 8 || paths.iter().any(|p| !p.is_absolute()) {
        return 0;
    }
    unsafe { &*stream }.state.lock().unwrap().exclusions = paths;
    1
}
