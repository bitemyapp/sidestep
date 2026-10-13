//! Synchronous file handles. A mutex serializes access to the shared file offset.
use crate::thread::lock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSData, NSError, NSString, NSURL};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::sync::Mutex;

sidestep_runtime::static_class!(pub(crate) NSFILEHANDLE, NSFILEHANDLE_META = "NSFileHandle", || { let _ = Handle::class(); });

struct State {
    file: Option<File>,
    close_on_drop: bool,
}
impl Drop for State {
    fn drop(&mut self) {
        if !self.close_on_drop
            && let Some(file) = self.file.take()
        {
            let _ = file.into_raw_fd();
        }
    }
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSFileHandle"]
    #[ivars = Mutex<State>]
    struct Handle;
    impl Handle {
        #[unsafe(method_id(initWithFileDescriptor:closeOnDealloc:))]
        fn init_fd(this: Allocated<Self>, fd: i32, close: bool) -> Retained<Self> {
            // The caller transfers responsibility for this descriptor, per NSFileHandle's contract.
            let this = this.set_ivars(Mutex::new(State { file: Some(unsafe { File::from_raw_fd(fd) }), close_on_drop: close }));
            unsafe { msg_send![super(this), init] }
        }
        #[unsafe(method_id(initWithFileDescriptor:))]
        fn init_fd_default(this: Allocated<Self>, fd: i32) -> Retained<Self> { unsafe { msg_send![this, initWithFileDescriptor: fd, closeOnDealloc: false] } }
        #[unsafe(method_id(fileHandleForReadingFromURL:error:))]
        fn reading(url: &NSURL, error: *mut *mut NSError) -> Option<Retained<Self>> { open(url, true, false, error) }
        #[unsafe(method_id(fileHandleForWritingToURL:error:))]
        fn writing(url: &NSURL, error: *mut *mut NSError) -> Option<Retained<Self>> { open(url, false, true, error) }
        #[unsafe(method_id(fileHandleForUpdatingURL:error:))]
        fn updating(url: &NSURL, error: *mut *mut NSError) -> Option<Retained<Self>> { open(url, true, true, error) }
        #[unsafe(method_id(fileHandleForReadingAtPath:))]
        fn reading_path(path: &NSString) -> Option<Retained<Self>> { open(&NSURL::fileURLWithPath(path), true, false, std::ptr::null_mut()) }
        #[unsafe(method(fileDescriptor))]
        fn descriptor(&self) -> i32 { lock(self.ivars()).file.as_ref().map_or(-1, AsRawFd::as_raw_fd) }
        #[unsafe(method_id(readDataUpToLength:error:))]
        fn read(&self, length: usize, error: *mut *mut NSError) -> Option<Retained<NSData>> { read(self, Some(length), error) }
        #[unsafe(method_id(readDataToEndOfFileAndReturnError:))]
        fn read_end(&self, error: *mut *mut NSError) -> Option<Retained<NSData>> { read(self, None, error) }
        #[unsafe(method_id(readDataOfLength:))]
        fn read_old(&self, length: usize) -> Option<Retained<NSData>> { read(self, Some(length), std::ptr::null_mut()) }
        #[unsafe(method_id(readDataToEndOfFile))]
        fn read_end_old(&self) -> Option<Retained<NSData>> { read(self, None, std::ptr::null_mut()) }
        #[unsafe(method(writeData:error:))]
        fn write(&self, data: &NSData, error: *mut *mut NSError) -> bool { result(with_file(self, |f| f.write_all(&data.to_vec())), error).is_some() }
        #[unsafe(method(seekToOffset:error:))]
        fn seek(&self, offset: u64, error: *mut *mut NSError) -> bool { result(with_file(self, |f| f.seek(SeekFrom::Start(offset))), error).is_some() }
        #[unsafe(method(getOffset:error:))]
        fn offset(&self, offset: *mut u64, error: *mut *mut NSError) -> bool {
            if let Some(value) = result(with_file(self, |f| f.stream_position()), error) { if !offset.is_null() { unsafe { offset.write(value) }; } true } else { false }
        }
        #[unsafe(method(truncateAtOffset:error:))]
        fn truncate(&self, offset: u64, error: *mut *mut NSError) -> bool { result(with_file(self, |f| f.set_len(offset)), error).is_some() }
        #[unsafe(method(synchronizeAndReturnError:))]
        fn sync(&self, error: *mut *mut NSError) -> bool { result(with_file(self, |f| f.sync_all()), error).is_some() }
        #[unsafe(method(closeAndReturnError:))]
        fn close(&self, error: *mut *mut NSError) -> bool { close(self, error) }
        #[unsafe(method(closeFile))]
        fn close_old(&self) { close(self, std::ptr::null_mut()); }
    }
    unsafe impl NSObjectProtocol for Handle {}
);
fn result<T>(r: io::Result<T>, error: *mut *mut NSError) -> Option<T> {
    match r {
        Ok(value) => Some(value),
        Err(e) => {
            unsafe { crate::error::set(error, crate::error::posix(e.raw_os_error().unwrap_or(libc::EIO))) };
            None
        }
    }
}
fn open(url: &NSURL, read: bool, write: bool, error: *mut *mut NSError) -> Option<Retained<Handle>> {
    let path = crate::url::file_path(url);
    let file = result(
        path.ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))
            .and_then(|p| OpenOptions::new().read(read).write(write).open(p)),
        error,
    )?;
    let this = Handle::alloc().set_ivars(Mutex::new(State { file: Some(file), close_on_drop: true }));
    Some(unsafe { msg_send![super(this), init] })
}
fn with_file<T>(handle: &Handle, f: impl FnOnce(&mut File) -> io::Result<T>) -> io::Result<T> {
    let mut state = lock(handle.ivars());
    f(state.file.as_mut().ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?)
}
fn read(handle: &Handle, length: Option<usize>, error: *mut *mut NSError) -> Option<Retained<NSData>> {
    result(
        with_file(handle, |f| {
            let mut bytes = Vec::new();
            match length {
                Some(n) => {
                    f.take(n as u64).read_to_end(&mut bytes)?;
                }
                None => {
                    f.read_to_end(&mut bytes)?;
                }
            };
            Ok(bytes)
        }),
        error,
    )
    .map(|bytes| NSData::with_bytes(&bytes))
}
fn close(handle: &Handle, error: *mut *mut NSError) -> bool {
    // Remove ownership before close: retrying close after EINTR can close a reused descriptor.
    let file = lock(handle.ivars()).file.take();
    let Some(file) = file else { return true };
    let fd = file.into_raw_fd();
    result(if unsafe { libc::close(fd) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }, error).is_some()
}
