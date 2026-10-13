//! CoreServices through objc2's public bindings. These tests exercise real
//! files and use deadlines, without depending on batch sizes or event counts.
#![allow(deprecated)]

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2_core_foundation::{CFArray, CFGetTypeID, CFRunLoop, CFString, ConcreteType, kCFRunLoopDefaultMode};
use objc2_core_services::*;
use sidestep as _;

static UNIQUE: AtomicUsize = AtomicUsize::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "sidestep-fsevents-{}-{}",
            std::process::id(),
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p.canonicalize().unwrap())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
struct Change {
    path: PathBuf,
    flags: u32,
    id: u64,
}
struct Client {
    tx: mpsc::Sender<Vec<Change>>,
    cf: bool,
    retains: AtomicUsize,
    releases: AtomicUsize,
    stop_in_callback: AtomicBool,
}
unsafe extern "C-unwind" fn retain(info: *const c_void) -> *const c_void {
    let client = unsafe { &*info.cast::<Client>() };
    client.retains.fetch_add(1, Ordering::SeqCst);
    unsafe { Arc::increment_strong_count(info.cast::<Client>()) };
    info
}
unsafe extern "C-unwind" fn release(info: *const c_void) {
    unsafe { &*info.cast::<Client>() }.releases.fetch_add(1, Ordering::SeqCst);
    unsafe { Arc::decrement_strong_count(info.cast::<Client>()) };
}
unsafe extern "C-unwind" fn callback(
    stream: ConstFSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: NonNull<c_void>,
    flags: NonNull<u32>,
    ids: NonNull<u64>,
) {
    let client = unsafe { &*info.cast::<Client>() };
    if client.stop_in_callback.load(Ordering::SeqCst) {
        unsafe {
            FSEventStreamStop(stream.cast_mut());
        }
    }
    let flags = unsafe { std::slice::from_raw_parts(flags.as_ptr(), count) };
    let ids = unsafe { std::slice::from_raw_parts(ids.as_ptr(), count) };
    let paths: Vec<PathBuf> = if client.cf {
        unsafe { paths.cast::<CFArray<CFString>>().as_ref() }
            .to_vec()
            .iter()
            .map(|p| PathBuf::from(p.to_string()))
            .collect()
    } else {
        unsafe { std::slice::from_raw_parts(paths.cast::<*const c_char>().as_ptr(), count) }
            .iter()
            .map(|p| PathBuf::from(unsafe { CStr::from_ptr(*p) }.to_str().unwrap()))
            .collect()
    };
    let _ = client.tx.send(
        paths
            .into_iter()
            .zip(flags)
            .zip(ids)
            .map(|((path, flags), id)| Change { path, flags: *flags, id: *id })
            .collect(),
    );
}
struct Watch {
    raw: FSEventStreamRef,
    client: Arc<Client>,
    rx: mpsc::Receiver<Vec<Change>>,
    queue: DispatchRetained<DispatchQueue>,
}
impl Watch {
    fn new(path: &Path, flags: u32) -> Self {
        let (tx, rx) = mpsc::channel();
        let client = Arc::new(Client {
            tx,
            cf: flags & kFSEventStreamCreateFlagUseCFTypes != 0,
            retains: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            stop_in_callback: AtomicBool::new(false),
        });
        let mut context = FSEventStreamContext {
            version: 0,
            info: Arc::as_ptr(&client).cast_mut().cast(),
            retain: Some(retain),
            release: Some(release),
            copyDescription: None,
        };
        let paths = CFArray::from_retained_objects(&[CFString::from_str(path.to_str().unwrap())]);
        let raw = unsafe {
            FSEventStreamCreate(
                None,
                Some(callback),
                &mut context,
                paths.as_ref(),
                kFSEventStreamEventIdSinceNow,
                0.02,
                flags,
            )
        };
        assert!(!raw.is_null(), "create stream");
        let queue = DispatchQueue::new("sidestep.fsevents.test", None);
        unsafe {
            FSEventStreamSetDispatchQueue(raw, Some(&queue));
        }
        Self { raw, client, rx, queue }
    }
    fn start(&self) {
        assert!(unsafe { FSEventStreamStart(self.raw) });
    }
    fn until(&self, path: &Path, flag: u32) -> Change {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let batch =
                self.rx.recv_timeout(end.saturating_duration_since(Instant::now())).expect("event before deadline");
            if let Some(event) = batch.into_iter().find(|e| e.path == path && e.flags & flag == flag) {
                return event;
            }
        }
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            FSEventStreamStop(self.raw);
            FSEventStreamInvalidate(self.raw);
            FSEventStreamRelease(self.raw);
        }
        self.queue.exec_sync(|| {});
    }
}

#[test]
fn recursive_file_events_report_create_write_rename_and_remove() {
    let dir = Scratch::new();
    std::fs::create_dir(dir.0.join("nested")).unwrap();
    let watch = Watch::new(&dir.0, kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer);
    watch.start();
    let file = dir.0.join("nested/notes.md");
    std::fs::write(&file, "one").unwrap();
    let created = watch.until(&file, kFSEventStreamEventFlagItemCreated);
    assert_ne!(created.flags & kFSEventStreamEventFlagItemIsFile, 0);
    std::fs::write(&file, "two").unwrap();
    let changed = watch.until(&file, kFSEventStreamEventFlagItemModified);
    assert!(changed.id >= created.id);
    let renamed = dir.0.join("nested/renamed.md");
    std::fs::rename(&file, &renamed).unwrap();
    watch.until(&renamed, kFSEventStreamEventFlagItemRenamed);
    std::fs::remove_file(&renamed).unwrap();
    watch.until(&renamed, kFSEventStreamEventFlagItemRemoved);
    assert!(unsafe { FSEventStreamGetLatestEventId(watch.raw) } <= unsafe { FSEventsGetCurrentEventId() });
    let paths = unsafe { FSEventStreamCopyPathsBeingWatched(watch.raw) };
    assert_eq!(paths.len(), 1);
    assert!(!unsafe { FSEventStreamCopyDescription(watch.raw) }.to_string().is_empty());
}

#[test]
fn cf_paths_and_atomic_replacement_reach_the_callback() {
    let dir = Scratch::new();
    let destination = dir.0.join("résumé.md");
    std::fs::write(&destination, "old").unwrap();
    let watch = Watch::new(&dir.0, kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagUseCFTypes);
    watch.start();
    let temporary = dir.0.join(".save");
    std::fs::write(&temporary, "new").unwrap();
    std::fs::rename(&temporary, &destination).unwrap();
    watch.until(&destination, kFSEventStreamEventFlagItemRenamed);
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "new");
}

#[test]
fn stream_restarts_and_releases_its_context_once() {
    let dir = Scratch::new();
    let watch = Watch::new(&dir.0, kFSEventStreamCreateFlagFileEvents);
    let client = watch.client.clone();
    assert_eq!(client.retains.load(Ordering::SeqCst), 1);
    watch.start();
    let first = dir.0.join("first.md");
    std::fs::write(&first, "a").unwrap();
    watch.until(&first, kFSEventStreamEventFlagItemCreated);
    unsafe {
        FSEventStreamStop(watch.raw);
    }
    watch.start();
    let second = dir.0.join("second.md");
    std::fs::write(&second, "b").unwrap();
    watch.until(&second, kFSEventStreamEventFlagItemCreated);
    unsafe {
        FSEventStreamFlushSync(watch.raw);
    }
    drop(watch);
    assert_eq!(client.releases.load(Ordering::SeqCst), 1);
}

#[test]
fn directory_events_use_directory_paths() {
    let dir = Scratch::new();
    let watch = Watch::new(&dir.0, 0);
    watch.start();
    std::fs::write(dir.0.join("notes.md"), "text").unwrap();
    watch.until(&dir.0, 0);
}

#[test]
fn watch_root_reports_renaming_a_watched_directory() {
    let dir = Scratch::new();
    let root = dir.0.join("watched");
    std::fs::create_dir(&root).unwrap();
    let watch = Watch::new(&root, kFSEventStreamCreateFlagWatchRoot | kFSEventStreamCreateFlagFileEvents);
    watch.start();
    std::fs::rename(&root, dir.0.join("moved")).unwrap();
    let event = watch.until(&root, kFSEventStreamEventFlagRootChanged);
    assert_eq!(event.id, 0);
}

#[test]
fn file_events_identify_symbolic_links() {
    let dir = Scratch::new();
    let target = dir.0.join("target.md");
    std::fs::write(&target, "text").unwrap();
    let watch = Watch::new(&dir.0, kFSEventStreamCreateFlagFileEvents);
    watch.start();
    let link = dir.0.join("link.md");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let event = watch.until(&link, kFSEventStreamEventFlagItemCreated);
    assert_ne!(event.flags & kFSEventStreamEventFlagItemIsSymlink, 0);
}

#[test]
fn exclusion_paths_suppress_events_inside_the_excluded_directory() {
    let dir = Scratch::new();
    let excluded = dir.0.join("excluded");
    std::fs::create_dir(&excluded).unwrap();
    let watch = Watch::new(&dir.0, kFSEventStreamCreateFlagFileEvents);
    let paths = CFArray::from_retained_objects(&[CFString::from_str(excluded.to_str().unwrap())]);
    assert!(unsafe { FSEventStreamSetExclusionPaths(watch.raw, paths.as_ref()) });
    watch.start();
    std::fs::write(excluded.join("hidden.md"), "text").unwrap();
    let kept = dir.0.join("kept.md");
    std::fs::write(&kept, "text").unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let batch = watch.rx.recv_timeout(end.saturating_duration_since(Instant::now())).unwrap();
        assert!(batch.iter().all(|event| !event.path.starts_with(&excluded)));
        if batch.iter().any(|event| event.path == kept) {
            break;
        }
    }
    unsafe { FSEventStreamFlushSync(watch.raw) };
    assert!(watch.rx.try_iter().flatten().all(|event| !event.path.starts_with(&excluded)));
}

#[test]
fn events_can_be_delivered_on_a_run_loop() {
    let dir = Scratch::new();
    let watch = Watch::new(&dir.0, kFSEventStreamCreateFlagFileEvents);
    let run_loop = CFRunLoop::current().unwrap();
    let mode = unsafe { kCFRunLoopDefaultMode }.unwrap();
    unsafe {
        FSEventStreamSetDispatchQueue(watch.raw, None);
        FSEventStreamScheduleWithRunLoop(watch.raw, &run_loop, mode);
    }
    watch.start();
    let path = dir.0.join("runloop.md");
    std::fs::write(&path, "text").unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        CFRunLoop::run_in_mode(Some(mode), 0.01, false);
        if watch.rx.try_iter().flatten().any(|e| e.path == path) {
            break;
        }
        assert!(Instant::now() < end, "run-loop delivery timed out");
    }
    unsafe {
        FSEventStreamStop(watch.raw);
        FSEventStreamUnscheduleFromRunLoop(watch.raw, &run_loop, mode);
    }
}

#[test]
fn metadata_keys_and_item_ownership_work_through_core_foundation() {
    for key in unsafe { [kMDItemTitle, kMDItemTextContent, kMDItemKeywords, kMDItemKind] } {
        assert!(key.is_some());
    }
    let dir = Scratch::new();
    let path = dir.0.join("notes.md");
    std::fs::write(&path, "hello").unwrap();
    let path = CFString::from_str(path.to_str().unwrap());
    let item = unsafe { MDItemCreate(None, Some(&path)) }.expect("metadata item for a file");
    assert_eq!(CFGetTypeID(Some(&item)), MDItem::type_id());
    let same = unsafe { MDItemCreate(None, Some(&path)) }.unwrap();
    assert_eq!(item, same);
    assert!(unsafe { MDItemCopyAttribute(&item, Some(&CFString::from_str("sidestep.unknown"))) }.is_none());
    let missing = CFString::from_str(dir.0.join("missing.md").to_str().unwrap());
    assert!(unsafe { MDItemCreate(None, Some(&missing)) }.is_none());
}

#[test]
fn stopping_from_the_stream_callback_does_not_deadlock() {
    let dir = Scratch::new();
    let watch = Watch::new(&dir.0, kFSEventStreamCreateFlagFileEvents);
    watch.client.stop_in_callback.store(true, Ordering::SeqCst);
    watch.start();
    let path = dir.0.join("stop.md");
    std::fs::write(&path, "text").unwrap();
    watch.until(&path, kFSEventStreamEventFlagItemCreated);
    watch.queue.exec_sync(|| {});
}

#[test]
fn stopping_cancels_batches_waiting_on_the_dispatch_queue() {
    let dir = Scratch::new();
    let watch = Watch::new(&dir.0, kFSEventStreamCreateFlagFileEvents);
    let (unblock, blocked) = mpsc::channel();
    let (ready, waiting) = mpsc::channel();
    watch.queue.exec_async(move || {
        ready.send(()).unwrap();
        blocked.recv().unwrap();
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    watch.start();
    std::fs::write(dir.0.join("queued.md"), "text").unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    let queued = loop {
        if unsafe { FSEventStreamFlushAsync(watch.raw) } > 0 {
            break true;
        }
        if Instant::now() >= end {
            break false;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    unsafe {
        FSEventStreamStop(watch.raw);
    }
    unblock.send(()).unwrap();
    watch.queue.exec_sync(|| {});
    assert!(queued, "an event was collected before stopping");
    assert!(watch.rx.try_recv().is_err(), "no stale callback after stop");
}
