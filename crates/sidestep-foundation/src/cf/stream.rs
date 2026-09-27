//! `CFReadStream` and `CFWriteStream`: files, memory, bound pairs and TCP
//! sockets, read and written as CoreFoundation's are, with a client's
//! events delivered on the run loops the stream is scheduled on.
//!
//! As measured on macOS (`conformance/tests/cf_types.rs`):
//!
//! - a stream is not open until opened; a file stream that can't open is
//!   in error (a POSIX error), as is a write that doesn't fit a fixed
//!   buffer (nothing of it is written);
//! - a file stream is at its end once a read returns nothing; a memory
//!   stream as soon as it has handed out its last byte (reading or through
//!   `CFReadStreamGetBuffer`, which a file stream doesn't have);
//! - a bound pair's writer takes as much as the buffer has room for, and
//!   nothing once the reader closed (still open, with no error); its
//!   reader waits for bytes, and is at its end once the writer closes;
//! - closing a stream that never opened leaves it to be opened;
//! - socket streams connect when either is opened, on a thread of their
//!   own: the streams are opening until then;
//! - a client scheduled on a run loop hears that a stream opened, then
//!   that bytes are there (a reader) or that it takes bytes (a writer),
//!   again after each read or write that leaves it so, and that a reader
//!   reached its end or that either failed.
//!
//! Sidestep has no `NSInputStream` or `NSOutputStream`: streams are
//! objects of private classes.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSData, NSNumber, NSString};

use super::types::{CFTypeID, id, is_null_allocator, object, owned};
use crate::runloop::modes::Mode;

type CFIndex = isize;
type Boolean = u8;

mod status {
    use super::CFIndex;
    pub(super) const NOT_OPEN: CFIndex = 0;
    pub(super) const OPENING: CFIndex = 1;
    pub(super) const OPEN: CFIndex = 2;
    pub(super) const AT_END: CFIndex = 5;
    pub(super) const CLOSED: CFIndex = 6;
    pub(super) const ERROR: CFIndex = 7;
}

mod event {
    pub(super) const OPEN_COMPLETED: usize = 1;
    pub(super) const HAS_BYTES: usize = 2;
    pub(super) const CAN_ACCEPT: usize = 4;
    pub(super) const ERROR: usize = 8;
    pub(super) const END: usize = 16;
}

/// `kCFStreamErrorDomainPOSIX`.
const POSIX_DOMAIN: CFIndex = 1;

/// `CFStreamError`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct StreamError {
    domain: CFIndex,
    error: i32,
}

type Retain = unsafe extern "C-unwind" fn(*mut c_void) -> *mut c_void;
type Release = unsafe extern "C-unwind" fn(*mut c_void);

/// `CFStreamClientContext`.
#[repr(C)]
pub struct ClientContext {
    version: CFIndex,
    info: *mut c_void,
    retain: Option<Retain>,
    release: Option<Release>,
    copy_description: Option<unsafe extern "C-unwind" fn(*mut c_void) -> *mut c_void>,
}

type Callback = unsafe extern "C-unwind" fn(*mut c_void, usize, *mut c_void);

/// A client: the events it wants, its callback and its context's info
/// (retained through the context's callbacks while it is the client).
struct Client {
    events: usize,
    callback: Callback,
    info: usize,
    release: Option<Release>,
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Some(release) = self.release {
            // SAFETY: the info the context's retain callback took.
            unsafe { release(self.info as *mut c_void) };
        }
    }
}

/// A bound pair's buffer, and whether either end closed.
struct Pipe {
    state: Mutex<PipeState>,
    ready: Condvar,
}

struct PipeState {
    bytes: VecDeque<u8>,
    capacity: usize,
    writer_closed: bool,
    reader_closed: bool,
}

/// A socket pair's connection, made when either stream opens.
struct Socket {
    host: String,
    port: u32,
    state: Mutex<SocketState>,
    ready: Condvar,
}

struct SocketState {
    status: CFIndex,
    error: StreamError,
    stream: Option<std::net::TcpStream>,
    /// The streams to tell when the connection is made.
    waiting: Vec<(usize, bool)>,
}

enum Source {
    File { path: PathBuf, file: Option<std::fs::File>, seek: Option<u64>, append: bool },
    Memory { bytes: Vec<u8>, pos: usize },
    Buffer { ptr: usize, capacity: usize, len: usize },
    Allocated { bytes: Vec<u8> },
    Pipe(Arc<Pipe>),
    Socket(Arc<Socket>),
}

struct State {
    reader: bool,
    status: CFIndex,
    error: StreamError,
    source: Source,
    client: Option<Client>,
    scheduled: Vec<(usize, Mode)>,
    properties: Vec<(String, Retained<AnyObject>)>,
}

define_class!(
    /// A `CFReadStream`.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCFReadStream"]
    #[ivars = Mutex<State>]
    struct ReadStream;
);

define_class!(
    /// A `CFWriteStream`.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCFWriteStream"]
    #[ivars = Mutex<State>]
    struct WriteStream;
);

impl Drop for State {
    fn drop(&mut self) {
        if let Source::Pipe(pipe) = &self.source {
            let mut s = crate::thread::lock(&pipe.state);
            if self.reader {
                s.reader_closed = true
            } else {
                s.writer_closed = true
            }
            pipe.ready.notify_all();
        }
    }
}

fn make(reader: bool, source: Source) -> *mut c_void {
    let state = State {
        reader,
        status: status::NOT_OPEN,
        error: StreamError::default(),
        source,
        client: None,
        scheduled: Vec::new(),
        properties: Vec::new(),
    };
    if reader {
        let this = ReadStream::alloc().set_ivars(Mutex::new(state));
        // SAFETY: NSObject's initializer.
        let this: Retained<ReadStream> = unsafe { msg_send![super(this), init] };
        owned(this)
    } else {
        let this = WriteStream::alloc().set_ivars(Mutex::new(state));
        // SAFETY: NSObject's initializer.
        let this: Retained<WriteStream> = unsafe { msg_send![super(this), init] };
        owned(this)
    }
}

/// A stream's state.
fn state<'a>(cf: *const c_void) -> &'a Mutex<State> {
    // SAFETY: the callers' contracts: `cf` is a stream from `make`; both
    // classes keep the same ivars.
    unsafe {
        let object = &*cf.cast::<AnyObject>();
        match object.downcast_ref::<ReadStream>() {
            Some(r) => r.ivars(),
            None => (*cf.cast::<WriteStream>()).ivars(),
        }
    }
}

/// The stream's state, locked; callers hold the guard no longer than
/// their use of the stream.
fn lock<'a>(cf: *const c_void) -> std::sync::MutexGuard<'a, State> {
    crate::thread::lock(state(cf))
}

/// Send `events` to the stream's client on each run loop it is scheduled
/// on, from there, the stream held until then.
fn notify(cf: *const c_void, s: &State, events: usize) {
    let Some(client) = &s.client else { return };
    let events = events & client.events;
    if events == 0 {
        return;
    }
    for &(run_loop, mode) in &s.scheduled {
        // SAFETY: the stream is live; the block releases what it retains.
        let stream = unsafe { objc2::ffi::objc_retain(cf.cast_mut().cast()) } as usize;
        let shared = crate::runloop::RunLoop(
            // SAFETY: a run loop the stream was scheduled on, kept alive by
            // the process (loops live as long as their threads).
            unsafe { crate::runloop::nsrunloop::CfRunLoopImpl::shared_of(run_loop as *const _) }.clone(),
        );
        shared.perform(&[mode], move || {
            for bit in [event::OPEN_COMPLETED, event::HAS_BYTES, event::CAN_ACCEPT, event::ERROR, event::END] {
                if events & bit == 0 {
                    continue;
                }
                // Read the client now: it may have changed, or gone.
                let client = {
                    let s = lock(stream as *const c_void);
                    s.client.as_ref().filter(|c| c.events & bit != 0).map(|c| (c.callback, c.info))
                };
                if let Some((callback, info)) = client {
                    // SAFETY: the client's callback with its info and the
                    // stream.
                    unsafe { callback(stream as *mut c_void, bit, info as *mut c_void) };
                }
            }
            // SAFETY: the reference taken above.
            unsafe { objc2::ffi::objc_release(stream as *mut _) };
        });
    }
}

fn fail(cf: *const c_void, s: &mut State, errno: i32) {
    s.status = status::ERROR;
    s.error = StreamError { domain: POSIX_DOMAIN, error: errno };
    notify(cf, s, event::ERROR);
}

fn io_errno(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(libc::EIO)
}

// Making streams.

/// # Safety
///
/// `url` is null or a file URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamCreateWithFile(_alloc: *const c_void, url: *const c_void) -> *mut c_void {
    // SAFETY: per this function's contract.
    let Some(path) = (!url.is_null()).then(|| crate::url::file_path(unsafe { &*url.cast() })).flatten() else {
        return std::ptr::null_mut();
    };
    make(true, Source::File { path, file: None, seek: None, append: false })
}

/// # Safety
///
/// `url` is null or a file URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamCreateWithFile(_alloc: *const c_void, url: *const c_void) -> *mut c_void {
    // SAFETY: per this function's contract.
    let Some(path) = (!url.is_null()).then(|| crate::url::file_path(unsafe { &*url.cast() })).flatten() else {
        return std::ptr::null_mut();
    };
    make(false, Source::File { path, file: None, seek: None, append: false })
}

/// A stream of the bytes. They are copied; unless the deallocator is
/// `kCFAllocatorNull`, they are `malloc`'s and freed now.
///
/// # Safety
///
/// `bytes` points to `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamCreateWithBytesNoCopy(
    _alloc: *const c_void,
    bytes: *const u8,
    length: CFIndex,
    deallocator: *const c_void,
) -> *mut c_void {
    let copied = if bytes.is_null() || length <= 0 {
        Vec::new()
    } else {
        // SAFETY: per this function's contract.
        unsafe { std::slice::from_raw_parts(bytes, length as usize) }.to_vec()
    };
    if !bytes.is_null() && !is_null_allocator(deallocator) {
        // SAFETY: a malloc'd buffer the stream owns, per the contract.
        unsafe { libc::free(bytes.cast_mut().cast()) };
    }
    make(true, Source::Memory { bytes: copied, pos: 0 })
}

/// # Safety
///
/// `buffer` has room for `capacity` bytes while the stream writes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamCreateWithBuffer(
    _alloc: *const c_void,
    buffer: *mut u8,
    capacity: CFIndex,
) -> *mut c_void {
    make(false, Source::Buffer { ptr: buffer as usize, capacity: capacity.max(0) as usize, len: 0 })
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFWriteStreamCreateWithAllocatedBuffers(
    _alloc: *const c_void,
    _buffers: *const c_void,
) -> *mut c_void {
    make(false, Source::Allocated { bytes: Vec::new() })
}

/// # Safety
///
/// `read` and `write` are null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStreamCreateBoundPair(
    _alloc: *const c_void,
    read: *mut *mut c_void,
    write: *mut *mut c_void,
    capacity: CFIndex,
) {
    let pipe = Arc::new(Pipe {
        state: Mutex::new(PipeState {
            bytes: VecDeque::new(),
            capacity: capacity.max(1) as usize,
            writer_closed: false,
            reader_closed: false,
        }),
        ready: Condvar::new(),
    });
    // SAFETY: per this function's contract.
    unsafe {
        if !read.is_null() {
            read.write(make(true, Source::Pipe(pipe.clone())));
        }
        if !write.is_null() {
            write.write(make(false, Source::Pipe(pipe)));
        }
    }
}

/// # Safety
///
/// `host` is null or a string; `read` and `write` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStreamCreatePairWithSocketToHost(
    _alloc: *const c_void,
    host: *const c_void,
    port: u32,
    read: *mut *mut c_void,
    write: *mut *mut c_void,
) {
    // SAFETY: per this function's contract.
    let host = if host.is_null() { String::new() } else { super::string::text(unsafe { object(host) }).into_owned() };
    let socket = Arc::new(Socket {
        host,
        port,
        state: Mutex::new(SocketState {
            status: status::NOT_OPEN,
            error: StreamError::default(),
            stream: None,
            waiting: Vec::new(),
        }),
        ready: Condvar::new(),
    });
    // SAFETY: per this function's contract.
    unsafe {
        if !read.is_null() {
            read.write(make(true, Source::Socket(socket.clone())));
        }
        if !write.is_null() {
            write.write(make(false, Source::Socket(socket)));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFReadStreamGetTypeID() -> CFTypeID {
    id::READ_STREAM
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFWriteStreamGetTypeID() -> CFTypeID {
    id::WRITE_STREAM
}

// Opening and closing.

/// Open a stream: a file now, a socket on a thread of its own.
fn open(cf: *const c_void) -> Boolean {
    let mut s = lock(cf);
    if s.status != status::NOT_OPEN {
        return 0;
    }
    let reader = s.reader;
    let result: Result<(), i32> = match &mut s.source {
        Source::File { path, file, seek, append } => {
            let opened = if reader {
                std::fs::File::open(&*path)
            } else {
                std::fs::OpenOptions::new().write(true).create(true).append(*append).truncate(!*append).open(&*path)
            };
            match opened {
                Ok(mut f) => {
                    let sought = seek.take().map(|at| f.seek(SeekFrom::Start(at)));
                    *file = Some(f);
                    sought.map_or(Ok(()), |r| r.map(drop).map_err(|e| io_errno(&e)))
                }
                Err(e) => Err(io_errno(&e)),
            }
        }
        Source::Socket(socket) => {
            let socket = socket.clone();
            s.status = status::OPENING;
            drop(s);
            connect(&socket, cf, reader);
            return 1;
        }
        _ => Ok(()),
    };
    match result {
        Ok(()) => {
            s.status = status::OPEN;
            let ready = if reader { event::HAS_BYTES } else { event::CAN_ACCEPT };
            let more = if reader { has_bytes(&mut s) } else { true };
            notify(cf, &s, event::OPEN_COMPLETED | if more { ready } else { 0 });
            1
        }
        Err(errno) => {
            fail(cf, &mut s, errno);
            0
        }
    }
}

/// Connect a socket pair's socket, if no stream has started to, on a
/// thread of its own; tell `cf` once it has.
fn connect(socket: &Arc<Socket>, cf: *const c_void, reader: bool) {
    let mut state = crate::thread::lock(&socket.state);
    // SAFETY: the stream is live; the connection's thread releases it.
    let stream = unsafe { objc2::ffi::objc_retain(cf.cast_mut().cast()) } as usize;
    state.waiting.push((stream, reader));
    if state.status != status::NOT_OPEN {
        drop(state);
        settle(socket);
        return;
    }
    state.status = status::OPENING;
    drop(state);
    let socket = socket.clone();
    std::thread::spawn(move || {
        use std::net::ToSocketAddrs;
        let connected = (socket.host.as_str(), socket.port as u16)
            .to_socket_addrs()
            .and_then(|mut addresses| {
                addresses.next().ok_or_else(|| std::io::Error::from_raw_os_error(libc::EHOSTUNREACH))
            })
            .and_then(std::net::TcpStream::connect);
        let mut state = crate::thread::lock(&socket.state);
        match connected {
            Ok(stream) => {
                state.stream = Some(stream);
                state.status = status::OPEN;
            }
            Err(e) => {
                state.status = status::ERROR;
                state.error = StreamError { domain: POSIX_DOMAIN, error: io_errno(&e) };
            }
        }
        socket.ready.notify_all();
        drop(state);
        settle(&socket);
    });
}

/// Once a socket connected or failed, bring the streams waiting on it to
/// its status and tell their clients.
fn settle(socket: &Arc<Socket>) {
    let (status, error, waiting) = {
        let mut state = crate::thread::lock(&socket.state);
        if state.status == status::OPENING {
            return;
        }
        (state.status, state.error, std::mem::take(&mut state.waiting))
    };
    for (stream, reader) in waiting {
        let cf = stream as *const c_void;
        {
            let mut s = lock(cf);
            if s.status == status::OPENING {
                s.status = status;
                s.error = error;
                let events = if status == status::OPEN {
                    event::OPEN_COMPLETED | if reader { 0 } else { event::CAN_ACCEPT }
                } else {
                    event::ERROR
                };
                notify(cf, &s, events);
            }
        }
        // SAFETY: the reference `connect` took.
        unsafe { objc2::ffi::objc_release(stream as *mut _) };
    }
}

/// # Safety
///
/// `cf` is a read stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamOpen(cf: *const c_void) -> Boolean {
    open(cf)
}

/// # Safety
///
/// `cf` is a write stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamOpen(cf: *const c_void) -> Boolean {
    open(cf)
}

fn close(cf: *const c_void) {
    let mut s = lock(cf);
    // A stream that never opened can still be opened.
    if s.status == status::NOT_OPEN || s.status == status::CLOSED {
        return;
    }
    s.status = status::CLOSED;
    let reader = s.reader;
    match &mut s.source {
        Source::File { file, .. } => {
            if let Some(f) = file.as_mut() {
                let _ = f.flush();
            }
            *file = None;
        }
        Source::Pipe(pipe) => {
            let mut p = crate::thread::lock(&pipe.state);
            if reader {
                p.reader_closed = true
            } else {
                p.writer_closed = true
            }
            pipe.ready.notify_all();
        }
        Source::Socket(socket) => {
            if let Some(stream) = crate::thread::lock(&socket.state).stream.as_ref() {
                let _ = stream.shutdown(if reader { std::net::Shutdown::Read } else { std::net::Shutdown::Write });
            }
        }
        _ => {}
    }
}

/// # Safety
///
/// `cf` is a read stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamClose(cf: *const c_void) {
    close(cf);
}

/// # Safety
///
/// `cf` is a write stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamClose(cf: *const c_void) {
    close(cf);
}

// Status and errors.

/// # Safety
///
/// `cf` is a read stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamGetStatus(cf: *const c_void) -> CFIndex {
    lock(cf).status
}

/// # Safety
///
/// `cf` is a write stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamGetStatus(cf: *const c_void) -> CFIndex {
    lock(cf).status
}

/// # Safety
///
/// `cf` is a read stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamGetError(cf: *const c_void) -> StreamError {
    lock(cf).error
}

/// # Safety
///
/// `cf` is a write stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamGetError(cf: *const c_void) -> StreamError {
    lock(cf).error
}

fn copy_error(cf: *const c_void) -> *mut c_void {
    let error = lock(cf).error;
    if error.domain == 0 {
        return std::ptr::null_mut();
    }
    let domain = match error.domain {
        POSIX_DOMAIN => "NSPOSIXErrorDomain",
        2 => "NSOSStatusErrorDomain",
        _ => "kCFErrorDomainCFNetwork",
    };
    owned(crate::error::make(&NSString::from_str(domain), error.error as isize, &[]))
}

/// # Safety
///
/// `cf` is a read stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamCopyError(cf: *const c_void) -> *mut c_void {
    copy_error(cf)
}

/// # Safety
///
/// `cf` is a write stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamCopyError(cf: *const c_void) -> *mut c_void {
    copy_error(cf)
}

// Reading.

/// Whether a read would return bytes without waiting.
fn has_bytes(s: &mut State) -> bool {
    if s.status != status::OPEN {
        return false;
    }
    match &mut s.source {
        // A file has bytes until a read finds none.
        Source::File { file: Some(_), .. } => true,
        Source::Memory { bytes, pos } => *pos < bytes.len(),
        Source::Pipe(pipe) => {
            let p = crate::thread::lock(&pipe.state);
            !p.bytes.is_empty() || p.writer_closed
        }
        Source::Socket(socket) => {
            let state = crate::thread::lock(&socket.state);
            let mut probe = [0u8; 1];
            state.stream.as_ref().is_some_and(|stream| {
                let _ = stream.set_nonblocking(true);
                let ready = stream.peek(&mut probe).is_ok();
                let _ = stream.set_nonblocking(false);
                ready
            })
        }
        _ => false,
    }
}

/// # Safety
///
/// `cf` is a read stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamHasBytesAvailable(cf: *const c_void) -> Boolean {
    u8::from(has_bytes(&mut lock(cf)))
}

/// Read up to `length` bytes, waiting for some if none are there: how many
/// were read, 0 at the end, -1 on an error.
///
/// # Safety
///
/// `cf` is a read stream; `buffer` has room for `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamRead(cf: *const c_void, buffer: *mut u8, length: CFIndex) -> CFIndex {
    let want = length.max(0) as usize;
    // A socket still connecting: wait for it, unlocked.
    let socket = match &lock(cf).source {
        Source::Socket(socket) => Some(socket.clone()),
        _ => None,
    };
    if let Some(socket) = &socket {
        let mut state = crate::thread::lock(&socket.state);
        while state.status == status::OPENING {
            state = socket.ready.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        drop(state);
        settle(socket);
    }
    let mut s = lock(cf);
    match s.status {
        status::OPEN => {}
        status::AT_END => return 0,
        _ => return -1,
    }
    // SAFETY: per this function's contract.
    let out = unsafe { std::slice::from_raw_parts_mut(buffer, want) };
    let result: Result<usize, i32> = match &mut s.source {
        Source::File { file: Some(f), .. } => f.read(out).map_err(|e| io_errno(&e)),
        Source::Memory { bytes, pos } => {
            let n = want.min(bytes.len() - *pos);
            out[..n].copy_from_slice(&bytes[*pos..*pos + n]);
            *pos += n;
            Ok(n)
        }
        Source::Pipe(pipe) => {
            let pipe = pipe.clone();
            drop(s);
            let mut p = crate::thread::lock(&pipe.state);
            while p.bytes.is_empty() && !p.writer_closed {
                p = pipe.ready.wait(p).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            let n = want.min(p.bytes.len());
            for (slot, byte) in out.iter_mut().zip(p.bytes.drain(..n)) {
                *slot = byte;
            }
            pipe.ready.notify_all();
            drop(p);
            s = lock(cf);
            Ok(n)
        }
        Source::Socket(socket) => {
            let stream = crate::thread::lock(&socket.state).stream.as_ref().and_then(|t| t.try_clone().ok());
            match stream {
                Some(mut stream) => {
                    drop(s);
                    let read = stream.read(out).map_err(|e| io_errno(&e));
                    s = lock(cf);
                    read
                }
                None => Err(libc::ENOTCONN),
            }
        }
        _ => Err(libc::EBADF),
    };
    match result {
        Ok(0) if want > 0 => {
            s.status = status::AT_END;
            notify(cf, &s, event::END);
            0
        }
        Ok(n) => {
            // A memory stream knows its end as soon as it reaches it.
            let at_end = matches!(&s.source, Source::Memory { bytes, pos } if *pos == bytes.len());
            if at_end {
                s.status = status::AT_END;
                notify(cf, &s, event::END);
            } else if has_bytes(&mut s) {
                notify(cf, &s, event::HAS_BYTES);
            }
            n as CFIndex
        }
        Err(errno) => {
            fail(cf, &mut s, errno);
            -1
        }
    }
}

/// The memory stream's next bytes, up to `max`, which then count as read;
/// NULL for other streams.
///
/// # Safety
///
/// `cf` is a read stream; `count` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamGetBuffer(
    cf: *const c_void,
    max: CFIndex,
    count: *mut CFIndex,
) -> *const u8 {
    let mut s = lock(cf);
    let open = s.status == status::OPEN;
    let (ptr, n) = match &mut s.source {
        Source::Memory { bytes, pos } if open => {
            let n = (max.max(0) as usize).min(bytes.len() - *pos);
            // The stream keeps its bytes as long as it lives.
            let ptr = bytes[*pos..].as_ptr();
            *pos += n;
            (ptr, n)
        }
        _ => (std::ptr::null(), 0),
    };
    if matches!(&s.source, Source::Memory { bytes, pos } if *pos == bytes.len()) && open {
        s.status = status::AT_END;
    }
    if !count.is_null() {
        // SAFETY: per this function's contract.
        unsafe { count.write(n as CFIndex) };
    }
    ptr
}

// Writing.

/// # Safety
///
/// `cf` is a write stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamCanAcceptBytes(cf: *const c_void) -> Boolean {
    let s = lock(cf);
    if s.status != status::OPEN {
        return 0;
    }
    u8::from(match &s.source {
        Source::Buffer { capacity, len, .. } => len < capacity,
        Source::Pipe(pipe) => {
            let p = crate::thread::lock(&pipe.state);
            p.bytes.len() < p.capacity && !p.reader_closed
        }
        _ => true,
    })
}

/// Write `length` bytes: how many were written, -1 on an error.
///
/// # Safety
///
/// `cf` is a write stream; `bytes` points to `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamWrite(cf: *const c_void, bytes: *const u8, length: CFIndex) -> CFIndex {
    let n = length.max(0) as usize;
    let data = if n == 0 || bytes.is_null() {
        &[][..]
    } else {
        // SAFETY: per this function's contract.
        unsafe { std::slice::from_raw_parts(bytes, n) }
    };
    let socket = match &lock(cf).source {
        Source::Socket(socket) => Some(socket.clone()),
        _ => None,
    };
    if let Some(socket) = &socket {
        let mut state = crate::thread::lock(&socket.state);
        while state.status == status::OPENING {
            state = socket.ready.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        drop(state);
        settle(socket);
    }
    let mut s = lock(cf);
    if s.status != status::OPEN {
        return -1;
    }
    let result: Result<usize, i32> = match &mut s.source {
        Source::File { file: Some(f), .. } => f.write(data).map_err(|e| io_errno(&e)),
        Source::Buffer { ptr, capacity, len } => {
            if *len + n > *capacity {
                Err(libc::ENOMEM)
            } else {
                // SAFETY: the caller's buffer has room for `capacity` bytes.
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), (*ptr as *mut u8).add(*len), n) };
                *len += n;
                Ok(n)
            }
        }
        Source::Allocated { bytes } => {
            bytes.extend_from_slice(data);
            Ok(n)
        }
        Source::Pipe(pipe) => {
            let mut p = crate::thread::lock(&pipe.state);
            if p.reader_closed {
                // Nothing is written, and the stream stays open.
                return -1;
            } else {
                let room = p.capacity.saturating_sub(p.bytes.len()).min(n);
                p.bytes.extend(&data[..room]);
                pipe.ready.notify_all();
                Ok(room)
            }
        }
        Source::Socket(socket) => {
            let stream = crate::thread::lock(&socket.state).stream.as_ref().and_then(|t| t.try_clone().ok());
            match stream {
                Some(mut stream) => stream.write(data).map_err(|e| io_errno(&e)),
                None => Err(libc::ENOTCONN),
            }
        }
        _ => Err(libc::EBADF),
    };
    match result {
        Ok(written) => {
            let full = match &s.source {
                Source::Buffer { capacity, len, .. } => len >= capacity,
                Source::Pipe(pipe) => {
                    let p = crate::thread::lock(&pipe.state);
                    p.bytes.len() >= p.capacity
                }
                _ => false,
            };
            if !full {
                notify(cf, &s, event::CAN_ACCEPT);
            }
            written as CFIndex
        }
        Err(errno) => {
            fail(cf, &mut s, errno);
            -1
        }
    }
}

// Properties.

fn copy_property(cf: *const c_void, name: *const c_void) -> *mut c_void {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the callers' contracts: `name` is a string.
    let key = super::string::text(unsafe { object(name) }).into_owned();
    let mut s = lock(cf);
    let value: Option<Retained<AnyObject>> = match (key.as_str(), &mut s.source) {
        ("kCFStreamPropertyFileCurrentOffset", Source::File { file: Some(f), .. }) => {
            f.stream_position().ok().map(|at| NSNumber::new_i64(at as i64).into())
        }
        ("kCFStreamPropertyFileCurrentOffset", Source::File { seek, .. }) => {
            Some(NSNumber::new_i64(seek.unwrap_or(0) as i64).into())
        }
        ("kCFStreamPropertyDataWritten", Source::Allocated { bytes }) => Some(NSData::with_bytes(bytes).into()),
        ("kCFStreamPropertySocketRemoteHostName", Source::Socket(socket)) => {
            Some(NSString::from_str(&socket.host).into())
        }
        ("kCFStreamPropertySocketRemotePortNumber", Source::Socket(socket)) => {
            Some(NSNumber::new_i64(i64::from(socket.port)).into())
        }
        ("kCFStreamPropertySocketNativeHandle", Source::Socket(socket)) => {
            use std::os::fd::AsRawFd;
            crate::thread::lock(&socket.state)
                .stream
                .as_ref()
                .map(|t| NSData::with_bytes(&t.as_raw_fd().to_ne_bytes()).into())
        }
        _ => s.properties.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone()),
    };
    value.map_or(std::ptr::null_mut(), owned)
}

fn set_property(cf: *const c_void, name: *const c_void, value: *const c_void) -> Boolean {
    if name.is_null() {
        return 0;
    }
    // SAFETY: the callers' contracts: `name` is a string, `value` null or
    // an object.
    let (key, value) = unsafe { (super::string::text(object(name)).into_owned(), value.cast::<AnyObject>().as_ref()) };
    let mut s = lock(cf);
    let number = |v: Option<&AnyObject>| v.and_then(|v| v.downcast_ref::<NSNumber>()).map(|n| n.longLongValue());
    let is_open = s.status == status::OPEN;
    match (key.as_str(), &mut s.source) {
        ("kCFStreamPropertyFileCurrentOffset", Source::File { file, seek, .. }) => {
            let Some(at) = number(value).filter(|&at| at >= 0) else { return 0 };
            match file.as_mut() {
                Some(f) if is_open => u8::from(f.seek(SeekFrom::Start(at as u64)).is_ok()),
                _ => {
                    *seek = Some(at as u64);
                    1
                }
            }
        }
        ("kCFStreamPropertyAppendToFile", Source::File { append, file: None, .. }) => {
            *append = value.and_then(|v| v.downcast_ref::<NSNumber>()).is_some_and(|n| n.boolValue());
            1
        }
        (_, Source::Socket(_)) => {
            // Proxies and security settings are kept for the program to
            // read back; the socket is plain TCP.
            s.properties.retain(|(k, _)| *k != key);
            if let Some(value) = value {
                s.properties.push((key, objc2::Message::retain(value)));
            }
            1
        }
        _ => 0,
    }
}

/// # Safety
///
/// `cf` is a read stream; `name` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamCopyProperty(cf: *const c_void, name: *const c_void) -> *mut c_void {
    copy_property(cf, name)
}

/// # Safety
///
/// `cf` is a write stream; `name` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamCopyProperty(cf: *const c_void, name: *const c_void) -> *mut c_void {
    copy_property(cf, name)
}

/// # Safety
///
/// `cf` is a read stream; `name` null or a string; `value` null or an
/// object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamSetProperty(
    cf: *const c_void,
    name: *const c_void,
    value: *const c_void,
) -> Boolean {
    set_property(cf, name, value)
}

/// # Safety
///
/// `cf` is a write stream; `name` null or a string; `value` null or an
/// object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamSetProperty(
    cf: *const c_void,
    name: *const c_void,
    value: *const c_void,
) -> Boolean {
    set_property(cf, name, value)
}

// Clients and run loops.

/// # Safety
///
/// `callback` null or a function; `context` null or a context.
unsafe fn set_client(
    cf: *const c_void,
    events: usize,
    callback: Option<Callback>,
    context: *mut ClientContext,
) -> Boolean {
    let client = match (callback, events) {
        (Some(callback), events) if events != 0 && !context.is_null() => {
            // SAFETY: per this function's contract.
            let context = unsafe { &*context };
            let info = match context.retain {
                // SAFETY: the context's own retain callback.
                Some(retain) => unsafe { retain(context.info) },
                None => context.info,
            };
            Some(Client { events, callback, info: info as usize, release: context.release })
        }
        _ => None,
    };
    // Dropped (releasing its info) outside the lock.
    let old = std::mem::replace(&mut lock(cf).client, client);
    drop(old);
    1
}

/// # Safety
///
/// `cf` is a read stream; `callback` null or a function taking it; the
/// context null or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamSetClient(
    cf: *const c_void,
    events: usize,
    callback: Option<Callback>,
    context: *mut ClientContext,
) -> Boolean {
    // SAFETY: per this function's contract.
    unsafe { set_client(cf, events, callback, context) }
}

/// # Safety
///
/// As [`CFReadStreamSetClient`], for a write stream.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamSetClient(
    cf: *const c_void,
    events: usize,
    callback: Option<Callback>,
    context: *mut ClientContext,
) -> Boolean {
    // SAFETY: per this function's contract.
    unsafe { set_client(cf, events, callback, context) }
}

fn schedule(cf: *const c_void, run_loop: *const c_void, mode: *const c_void, add: bool) {
    if run_loop.is_null() || mode.is_null() {
        return;
    }
    // SAFETY: the callers' contracts: `mode` is a string.
    let mode = Mode::from_ns(unsafe { &*mode.cast::<NSString>() });
    let mut s = lock(cf);
    let entry = (run_loop as usize, mode);
    if add {
        if !s.scheduled.contains(&entry) {
            s.scheduled.push(entry);
        }
    } else {
        s.scheduled.retain(|e| *e != entry);
    }
}

/// # Safety
///
/// `cf` is a read stream; `run_loop` null or a run loop; `mode` null or a
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamScheduleWithRunLoop(
    cf: *const c_void,
    run_loop: *const c_void,
    mode: *const c_void,
) {
    schedule(cf, run_loop, mode, true);
}

/// # Safety
///
/// As [`CFReadStreamScheduleWithRunLoop`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFReadStreamUnscheduleFromRunLoop(
    cf: *const c_void,
    run_loop: *const c_void,
    mode: *const c_void,
) {
    schedule(cf, run_loop, mode, false);
}

/// # Safety
///
/// `cf` is a write stream; `run_loop` null or a run loop; `mode` null or a
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamScheduleWithRunLoop(
    cf: *const c_void,
    run_loop: *const c_void,
    mode: *const c_void,
) {
    schedule(cf, run_loop, mode, true);
}

/// # Safety
///
/// As [`CFWriteStreamScheduleWithRunLoop`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFWriteStreamUnscheduleFromRunLoop(
    cf: *const c_void,
    run_loop: *const c_void,
    mode: *const c_void,
) {
    schedule(cf, run_loop, mode, false);
}

macro_rules! properties {
    ($($name:ident = $value:literal,)*) => {
        $(crate::constant_string!($name = $value);)*
    };
}

properties! {
    kCFStreamPropertyAppendToFile = "kCFStreamPropertyAppendToFile",
    kCFStreamPropertyDataWritten = "kCFStreamPropertyDataWritten",
    kCFStreamPropertyFileCurrentOffset = "kCFStreamPropertyFileCurrentOffset",
    kCFStreamPropertySocketNativeHandle = "kCFStreamPropertySocketNativeHandle",
    kCFStreamPropertySocketRemoteHostName = "kCFStreamPropertySocketRemoteHostName",
    kCFStreamPropertySocketRemotePortNumber = "kCFStreamPropertySocketRemotePortNumber",
    kCFStreamPropertySOCKSPassword = "kCFStreamPropertySOCKSPassword",
    kCFStreamPropertySOCKSProxy = "kCFStreamPropertySOCKSProxy",
    kCFStreamPropertySOCKSProxyHost = "SOCKSProxy",
    kCFStreamPropertySOCKSProxyPort = "SOCKSPort",
    kCFStreamPropertySOCKSUser = "kCFStreamPropertySOCKSUser",
    kCFStreamPropertySOCKSVersion = "kCFStreamPropertySOCKSVersion",
    kCFStreamPropertyShouldCloseNativeSocket = "kCFStreamPropertyShouldCloseNativeSocket",
    kCFStreamPropertySocketSecurityLevel = "kCFStreamPropertySocketSecurityLevel",
    kCFStreamSocketSOCKSVersion4 = "kCFStreamSocketSOCKSVersion4",
    kCFStreamSocketSOCKSVersion5 = "kCFStreamSocketSOCKSVersion5",
    kCFStreamSocketSecurityLevelNegotiatedSSL = "kCFStreamSocketSecurityLevelNegotiatedSSL",
    kCFStreamSocketSecurityLevelNone = "kCFStreamSocketSecurityLevelNone",
    kCFStreamSocketSecurityLevelSSLv2 = "kCFStreamSocketSecurityLevelSSLv2",
    kCFStreamSocketSecurityLevelSSLv3 = "kCFStreamSocketSecurityLevelSSLv3",
    kCFStreamSocketSecurityLevelTLSv1 = "kCFStreamSocketSecurityLevelTLSv1",
}

#[unsafe(no_mangle)]
pub static kCFStreamErrorDomainSSL: i32 = 3;
#[unsafe(no_mangle)]
pub static kCFStreamErrorDomainSOCKS: i32 = 5;
