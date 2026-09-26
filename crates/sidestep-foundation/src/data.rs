//! `NSData` and `NSMutableData`.
//!
//! Both keep their bytes in one [`Storage`]: an owned boxed slice for
//! immutable data, the caller's buffer for the no-copy initializers (freed
//! with `free` or handed to the deallocator block, exactly once, when the
//! data goes), or a growable vector for mutable data. `-bytes` stays put
//! until the data is mutated, and empty immutable data has no bytes (NULL),
//! as on macOS. Mutable data made "without copying" copies anyway and
//! gives the buffer back at once, since it must be able to grow.
//!
//! Like Apple's, mutable data isn't safe to use from several threads at
//! once; immutable data is.
//!
//! [`bytes`] gives Rust code (AppKit's pasteboard) the contents of any
//! `NSData` without copying.

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::fmt::Write as _;
use std::ptr::NonNull;

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSData, NSError, NSMutableData, NSNotFound, NSRange, NSString, NSUInteger, NSURL, NSZone};

use crate::base64;

sidestep_runtime::static_class!(pub(crate) NSDATA, NSDATA_META = "NSData", || {
    let _ = NSDataImpl::class();
    crate::perform::install();
});

sidestep_runtime::static_class!(pub(crate) NSMUTABLEDATA, NSMUTABLEDATA_META = "NSMutableData", || {
    let _ = NSMutableDataImpl::class();
    crate::perform::install();
});

type Deallocator = RcBlock<dyn Fn(NonNull<c_void>, NSUInteger)>;

/// Who frees a caller's buffer.
enum Free {
    Nothing,
    Libc,
    Block(Deallocator),
}

enum Storage {
    Owned(Box<[u8]>),
    Borrowed { ptr: NonNull<u8>, len: usize, free: Free },
    Growable(Vec<u8>),
}

impl Storage {
    fn bytes(&self) -> &[u8] {
        match self {
            Storage::Owned(bytes) => bytes,
            // SAFETY: the caller gave us `len` readable bytes that live until
            // we free them.
            Storage::Borrowed { ptr, len, .. } => unsafe { std::slice::from_raw_parts(ptr.as_ptr(), *len) },
            Storage::Growable(bytes) => bytes,
        }
    }

    /// `-bytes`: NULL for empty immutable data.
    fn ptr(&self) -> *const c_void {
        match self {
            Storage::Owned(bytes) if bytes.is_empty() => std::ptr::null(),
            Storage::Owned(bytes) => bytes.as_ptr().cast(),
            Storage::Borrowed { ptr, .. } => ptr.as_ptr().cast(),
            Storage::Growable(bytes) => bytes.as_ptr().cast(),
        }
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        if let Storage::Borrowed { ptr, len, free } = self {
            match free {
                Free::Nothing => {}
                // SAFETY: the caller asked us to free() the buffer when done.
                Free::Libc => unsafe { libc::free(ptr.as_ptr().cast()) },
                Free::Block(block) => block.call((ptr.cast(), *len)),
            }
        }
    }
}

pub(crate) struct DataIvars {
    storage: UnsafeCell<Storage>,
}

impl DataIvars {
    fn new(storage: Storage) -> Self {
        DataIvars { storage: UnsafeCell::new(storage) }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSData"]
    #[ivars = DataIvars]
    pub(crate) struct NSDataImpl;

    impl NSDataImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_with(this, Vec::new())
        }

        #[unsafe(method_id(initWithBytes:length:))]
        fn init_with_bytes(this: Allocated<Self>, bytes: *const c_void, length: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `length` readable bytes.
            init_with(this, unsafe { raw(bytes, length) }.to_vec())
        }

        #[unsafe(method_id(initWithBytesNoCopy:length:))]
        fn init_no_copy(this: Allocated<Self>, bytes: NonNull<c_void>, length: NSUInteger) -> Retained<Self> {
            init_borrowed(this, bytes, length, Free::Libc)
        }

        #[unsafe(method_id(initWithBytesNoCopy:length:freeWhenDone:))]
        fn init_no_copy_free(
            this: Allocated<Self>,
            bytes: NonNull<c_void>,
            length: NSUInteger,
            free: bool,
        ) -> Retained<Self> {
            init_borrowed(this, bytes, length, if free { Free::Libc } else { Free::Nothing })
        }

        #[unsafe(method_id(initWithBytesNoCopy:length:deallocator:))]
        fn init_no_copy_deallocator(
            this: Allocated<Self>,
            bytes: NonNull<c_void>,
            length: NSUInteger,
            deallocator: Option<&DynBlock<dyn Fn(NonNull<c_void>, NSUInteger)>>,
        ) -> Retained<Self> {
            let free = deallocator.map_or(Free::Nothing, |d| Free::Block(d.copy()));
            init_borrowed(this, bytes, length, free)
        }

        #[unsafe(method_id(initWithData:))]
        fn init_with_data(this: Allocated<Self>, data: &NSData) -> Retained<Self> {
            init_with(this, bytes(data).to_vec())
        }

        #[unsafe(method_id(initWithContentsOfFile:))]
        fn init_with_contents_of_file(this: Allocated<Self>, path: &NSString) -> Option<Retained<Self>> {
            match std::fs::read(path.to_string()) {
                Ok(bytes) => Some(init_with(this, bytes)),
                Err(_) => None,
            }
        }

        #[unsafe(method_id(initWithContentsOfFile:options:error:))]
        fn init_with_contents_of_file_error(
            this: Allocated<Self>,
            path: &NSString,
            _options: NSUInteger,
            error: *mut *mut NSError,
        ) -> Option<Retained<Self>> {
            // SAFETY: the caller passes null or room for an error.
            init_read(this, unsafe { read_path(&path.to_string(), error) })
        }

        #[unsafe(method_id(initWithContentsOfURL:))]
        fn init_with_contents_of_url(this: Allocated<Self>, url: &NSURL) -> Option<Retained<Self>> {
            // SAFETY: no error out-parameter.
            init_read(this, unsafe { read_url(url, std::ptr::null_mut()) })
        }

        #[unsafe(method_id(initWithContentsOfURL:options:error:))]
        fn init_with_contents_of_url_error(
            this: Allocated<Self>,
            url: &NSURL,
            _options: NSUInteger,
            error: *mut *mut NSError,
        ) -> Option<Retained<Self>> {
            // SAFETY: the caller passes null or room for an error.
            init_read(this, unsafe { read_url(url, error) })
        }

        #[unsafe(method_id(dataWithContentsOfFile:options:error:))]
        fn data_with_contents_of_file_error(
            path: &NSString,
            _options: NSUInteger,
            error: *mut *mut NSError,
        ) -> Option<Retained<Self>> {
            // SAFETY: the caller passes null or room for an error.
            init_read(Self::alloc(), unsafe { read_path(&path.to_string(), error) })
        }

        #[unsafe(method_id(dataWithContentsOfURL:))]
        fn data_with_contents_of_url(url: &NSURL) -> Option<Retained<Self>> {
            // SAFETY: no error out-parameter.
            init_read(Self::alloc(), unsafe { read_url(url, std::ptr::null_mut()) })
        }

        #[unsafe(method_id(dataWithContentsOfURL:options:error:))]
        fn data_with_contents_of_url_error(
            url: &NSURL,
            _options: NSUInteger,
            error: *mut *mut NSError,
        ) -> Option<Retained<Self>> {
            // SAFETY: the caller passes null or room for an error.
            init_read(Self::alloc(), unsafe { read_url(url, error) })
        }

        #[unsafe(method_id(initWithBase64EncodedString:options:))]
        fn init_with_base64_string(this: Allocated<Self>, text: &NSString, options: NSUInteger) -> Option<Retained<Self>> {
            base64::decode(text.to_string().as_bytes(), options).map(|bytes| init_with(this, bytes))
        }

        #[unsafe(method_id(initWithBase64EncodedData:options:))]
        fn init_with_base64_data(this: Allocated<Self>, data: &NSData, options: NSUInteger) -> Option<Retained<Self>> {
            base64::decode(bytes(data), options).map(|bytes| init_with(this, bytes))
        }

        #[unsafe(method_id(data))]
        fn data() -> Retained<Self> {
            make(Vec::new())
        }

        #[unsafe(method_id(dataWithBytes:length:))]
        fn data_with_bytes(bytes: *const c_void, length: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `length` readable bytes.
            make(unsafe { raw(bytes, length) }.to_vec())
        }

        #[unsafe(method_id(dataWithBytesNoCopy:length:))]
        fn data_no_copy(bytes: NonNull<c_void>, length: NSUInteger) -> Retained<Self> {
            init_borrowed(Self::alloc(), bytes, length, Free::Libc)
        }

        #[unsafe(method_id(dataWithBytesNoCopy:length:freeWhenDone:))]
        fn data_no_copy_free(bytes: NonNull<c_void>, length: NSUInteger, free: bool) -> Retained<Self> {
            init_borrowed(Self::alloc(), bytes, length, if free { Free::Libc } else { Free::Nothing })
        }

        #[unsafe(method_id(dataWithData:))]
        fn data_with_data(data: &NSData) -> Retained<Self> {
            make(bytes(data).to_vec())
        }

        #[unsafe(method_id(dataWithContentsOfFile:))]
        fn data_with_contents_of_file(path: &NSString) -> Option<Retained<Self>> {
            std::fs::read(path.to_string()).ok().map(make)
        }

        #[unsafe(method(length))]
        fn length(&self) -> NSUInteger {
            self.contents().len()
        }

        #[unsafe(method(bytes))]
        fn bytes_method(&self) -> *const c_void {
            self.storage().ptr()
        }

        #[unsafe(method(getBytes:length:))]
        fn get_bytes_length(&self, buffer: NonNull<c_void>, length: NSUInteger) {
            let contents = self.contents();
            copy_out(buffer, &contents[..length.min(contents.len())]);
        }

        #[unsafe(method(getBytes:range:))]
        fn get_bytes_range(&self, buffer: NonNull<c_void>, range: NSRange) {
            copy_out(buffer, &self.contents()[checked(range, self.contents().len(), "getBytes:range:")]);
        }

        #[unsafe(method(isEqualToData:))]
        fn is_equal_to_data(&self, other: &NSData) -> bool {
            self.contents() == bytes(other)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSData>()).is_some_and(|o| self.contents() == bytes(o))
        }

        /// CoreFoundation's hash of data: an ELF hash of the first 80 bytes.
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            let mut h: u32 = 0;
            for &b in self.contents().iter().take(80) {
                h = (h << 4).wrapping_add(u32::from(b));
                let g = h & 0xF000_0000;
                if g != 0 {
                    h ^= g >> 24;
                }
                h &= !g;
            }
            h as NSUInteger
        }

        #[unsafe(method_id(subdataWithRange:))]
        fn subdata_with_range(&self, range: NSRange) -> Retained<NSData> {
            let sub = self.contents()[checked(range, self.contents().len(), "subdataWithRange:")].to_vec();
            as_data(make(sub))
        }

        #[unsafe(method(rangeOfData:options:range:))]
        fn range_of_data(&self, needle: &NSData, options: NSUInteger, range: NSRange) -> NSRange {
            let haystack = &self.contents()[checked(range, self.contents().len(), "rangeOfData:options:range:")];
            match find(haystack, bytes(needle), options & 1 != 0, options & 2 != 0) {
                Some(at) => NSRange::new(range.location + at, needle.length()),
                None => NSRange::new(NSNotFound as NSUInteger, 0),
            }
        }

        #[unsafe(method(writeToFile:atomically:))]
        fn write_to_file(&self, path: &NSString, atomically: bool) -> bool {
            let options = if atomically { WRITE_ATOMIC } else { 0 };
            write(&path.to_string(), self.contents(), options).is_ok()
        }

        #[unsafe(method(writeToURL:atomically:))]
        fn write_to_url(&self, url: &NSURL, atomically: bool) -> bool {
            let options = if atomically { WRITE_ATOMIC } else { 0 };
            // SAFETY: no error out-parameter.
            unsafe { write_url(self.contents(), url, options, std::ptr::null_mut()) }
        }

        #[unsafe(method(writeToFile:options:error:))]
        fn write_to_file_error(&self, path: &NSString, options: NSUInteger, error: *mut *mut NSError) -> bool {
            // SAFETY: the caller passes null or room for an error.
            unsafe { write_path(self.contents(), &path.to_string(), options, error) }
        }

        #[unsafe(method(writeToURL:options:error:))]
        fn write_to_url_error(&self, url: &NSURL, options: NSUInteger, error: *mut *mut NSError) -> bool {
            // SAFETY: the caller passes null or room for an error.
            unsafe { write_url(self.contents(), url, options, error) }
        }

        #[unsafe(method_id(base64EncodedStringWithOptions:))]
        fn base64_string(&self, options: NSUInteger) -> Retained<NSString> {
            NSString::from_str(&base64::encode(self.contents(), options))
        }

        #[unsafe(method_id(base64EncodedDataWithOptions:))]
        fn base64_data(&self, options: NSUInteger) -> Retained<NSData> {
            as_data(make(base64::encode(self.contents(), options).into_bytes()))
        }

        #[unsafe(method(enumerateByteRangesUsingBlock:))]
        fn enumerate_byte_ranges(&self, block: &DynBlock<dyn Fn(NonNull<c_void>, NSRange, NonNull<objc2::runtime::Bool>)>) {
            let contents = self.contents();
            if let Some(ptr) = NonNull::new(contents.as_ptr().cast_mut().cast::<c_void>())
                && !contents.is_empty()
            {
                let mut stop = objc2::runtime::Bool::NO;
                block.call((ptr, NSRange::new(0, contents.len()), NonNull::from(&mut stop)));
            }
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&describe(self.contents()))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSData> {
            if self.is_mutable() {
                as_data(make(self.contents().to_vec()))
            } else {
                // Immutable: a copy is the same object.
                as_data(self.retain())
            }
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMutableData> {
            make_mutable(self.contents().to_vec())
        }
    }

    unsafe impl NSObjectProtocol for NSDataImpl {}
);

impl NSDataImpl {
    fn storage(&self) -> &Storage {
        // SAFETY: storage is only replaced by mutable data's own methods,
        // which (as on macOS) must not race with readers.
        unsafe { &*self.ivars().storage.get() }
    }

    /// # Safety
    /// No reference from [`storage`] may be alive; mutable data isn't
    /// thread-safe.
    #[allow(clippy::mut_from_ref)]
    unsafe fn storage_mut(&self) -> &mut Storage {
        // SAFETY: guaranteed by the caller.
        unsafe { &mut *self.ivars().storage.get() }
    }

    fn contents(&self) -> &[u8] {
        self.storage().bytes()
    }

    fn is_mutable(&self) -> bool {
        matches!(self.storage(), Storage::Growable(_))
    }
}

/// `length` bytes at `bytes` (none for a null pointer with no length).
///
/// # Safety
/// `bytes` must point to `length` readable bytes unless `length` is 0.
unsafe fn raw<'a>(bytes: *const c_void, length: usize) -> &'a [u8] {
    if length == 0 {
        &[]
    } else {
        // SAFETY: guaranteed by the caller.
        unsafe { std::slice::from_raw_parts(bytes.cast(), length) }
    }
}

fn copy_out(buffer: NonNull<c_void>, bytes: &[u8]) {
    // SAFETY: the caller of getBytes: passes room for what it asked for.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.as_ptr().cast(), bytes.len()) };
}

/// The bytes a range covers, or the exception macOS raises.
fn checked(range: NSRange, len: usize, method: &str) -> std::ops::Range<usize> {
    match range.location.checked_add(range.length) {
        Some(end) if end <= len => range.location..end,
        _ => panic!("-[NSData {method}]: range {{{}, {}}} exceeds data length {len}", range.location, range.length),
    }
}

/// Whether an allocation is for `NSMutableData` (or a subclass).
fn is_mutable_class(object: &AnyObject) -> bool {
    let target = <NSMutableData as ClassType>::class();
    let mut class: Option<&AnyClass> = Some(object.class());
    while let Some(c) = class {
        if std::ptr::eq(c, target) {
            return true;
        }
        class = c.superclass();
    }
    false
}

fn init_with(this: Allocated<NSDataImpl>, bytes: Vec<u8>) -> Retained<NSDataImpl> {
    // SAFETY: an allocated, uninitialized object of this class or a subclass.
    let mutable = is_mutable_class(unsafe { &*Allocated::as_ptr(&this).cast::<AnyObject>() });
    let storage = if mutable { Storage::Growable(bytes) } else { Storage::Owned(bytes.into_boxed_slice()) };
    let this = this.set_ivars(DataIvars::new(storage));
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn init_borrowed(
    this: Allocated<NSDataImpl>,
    bytes: NonNull<c_void>,
    length: usize,
    free: Free,
) -> Retained<NSDataImpl> {
    // SAFETY: as above.
    let mutable = is_mutable_class(unsafe { &*Allocated::as_ptr(&this).cast::<AnyObject>() });
    let storage = Storage::Borrowed { ptr: bytes.cast(), len: length, free };
    if mutable {
        // Mutable data must be able to grow: copy, and give the buffer back
        // now.
        let copy = storage.bytes().to_vec();
        drop(storage);
        return init_with(this, copy);
    }
    let this = this.set_ivars(DataIvars::new(storage));
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// New immutable data. Only called once the class is loaded.
fn make(bytes: Vec<u8>) -> Retained<NSDataImpl> {
    init_with(NSDataImpl::alloc(), bytes)
}

fn make_mutable(bytes: Vec<u8>) -> Retained<NSMutableData> {
    // SAFETY: +alloc on the class NSMutableData names; initWithBytes:length:
    // copies.
    unsafe {
        let this: Allocated<NSMutableData> = msg_send![NSMutableData::class(), alloc];
        msg_send![this, initWithBytes: bytes.as_ptr().cast::<c_void>(), length: bytes.len()]
    }
}

fn as_data(data: Retained<NSDataImpl>) -> Retained<NSData> {
    // SAFETY: NSDataImpl is the class NSData names.
    unsafe { Retained::cast_unchecked(data) }
}

fn find(haystack: &[u8], needle: &[u8], backwards: bool, anchored: bool) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    match (backwards, anchored) {
        (false, true) => (haystack[..needle.len()] == *needle).then_some(0),
        (true, true) => (haystack[last..] == *needle).then_some(last),
        (false, false) => haystack.windows(needle.len()).position(|w| w == needle),
        (true, false) => haystack.windows(needle.len()).rposition(|w| w == needle),
    }
}

/// `{length = 3, bytes = 0x010203}`; past 24 bytes, the first 16 and the
/// last 8 in groups of four.
fn describe(bytes: &[u8]) -> String {
    let mut text = format!("{{length = {}, bytes = 0x", bytes.len());
    if bytes.len() <= 24 {
        for b in bytes {
            let _ = write!(text, "{b:02x}");
        }
    } else {
        let group = |text: &mut String, part: &[u8]| {
            for (i, b) in part.iter().enumerate() {
                let _ = write!(text, "{b:02x}");
                if i % 4 == 3 {
                    text.push(' ');
                }
            }
        };
        group(&mut text, &bytes[..16]);
        text.push_str("... ");
        group(&mut text, &bytes[bytes.len() - 8..]);
    }
    text.push('}');
    text
}

/// `NSDataWritingAtomic`.
const WRITE_ATOMIC: NSUInteger = 1;
/// `NSDataWritingWithoutOverwriting`.
const WRITE_WITHOUT_OVERWRITING: NSUInteger = 2;

fn init_read(this: Allocated<NSDataImpl>, bytes: Option<Vec<u8>>) -> Option<Retained<NSDataImpl>> {
    match bytes {
        Some(bytes) => Some(init_with(this, bytes)),
        None => {
            drop(this);
            None
        }
    }
}

/// Read a file, reporting failure in `error`.
///
/// # Safety
///
/// `error` is null or points to room for an error.
unsafe fn read_path(path: &str, error: *mut *mut NSError) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            // SAFETY: per this function's contract.
            unsafe { crate::error::set(error, crate::error::file(crate::error::FileOp::Read, &e, path)) };
            None
        }
    }
}

/// Read what a file URL names; other URLs are unsupported.
///
/// # Safety
///
/// As [`read_path`].
unsafe fn read_url(url: &NSURL, error: *mut *mut NSError) -> Option<Vec<u8>> {
    match crate::url::file_path(url) {
        // SAFETY: per this function's contract.
        Some(path) => unsafe { read_path(&path.to_string_lossy(), error) },
        None => {
            let info = [(&crate::error::URL, url.retain().into())];
            // SAFETY: per this function's contract.
            unsafe {
                crate::error::set(error, crate::error::cocoa(crate::error::code::FILE_READ_UNSUPPORTED_SCHEME, &info))
            };
            None
        }
    }
}

/// Write to a path with `NSDataWritingOptions`, reporting failure in
/// `error`.
///
/// # Safety
///
/// As [`read_path`].
unsafe fn write_path(bytes: &[u8], path: &str, options: NSUInteger, error: *mut *mut NSError) -> bool {
    match write(path, bytes, options) {
        Ok(()) => true,
        Err(e) => {
            // SAFETY: per this function's contract.
            unsafe { crate::error::set(error, crate::error::file(crate::error::FileOp::Write, &e, path)) };
            false
        }
    }
}

/// Write to what a file URL names; other URLs are unsupported.
///
/// # Safety
///
/// As [`read_path`].
unsafe fn write_url(bytes: &[u8], url: &NSURL, options: NSUInteger, error: *mut *mut NSError) -> bool {
    match crate::url::file_path(url) {
        // SAFETY: per this function's contract.
        Some(path) => unsafe { write_path(bytes, &path.to_string_lossy(), options, error) },
        None => {
            let info = [(&crate::error::URL, url.retain().into())];
            // SAFETY: per this function's contract.
            unsafe {
                crate::error::set(error, crate::error::cocoa(crate::error::code::FILE_WRITE_UNSUPPORTED_SCHEME, &info))
            };
            false
        }
    }
}

/// Write `bytes` to `path` with `NSDataWritingOptions`: atomically means
/// through a temporary file in the same directory, flushed and renamed
/// over the target; without overwriting fails if the file exists.
pub(crate) fn write(path: &str, bytes: &[u8], options: NSUInteger) -> std::io::Result<()> {
    use std::io::Write;
    if options & WRITE_WITHOUT_OVERWRITING != 0 {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        return file.write_all(bytes);
    }
    if options & WRITE_ATOMIC == 0 {
        return std::fs::write(path, bytes);
    }
    let target = std::path::Path::new(path);
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    let name = target.file_name().ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // Unique per write, so concurrent writers of one file don't share it.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = dir.join(format!(".{}.sidestep-{}-{n}", name.to_string_lossy(), std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

define_class!(
    #[unsafe(super(NSData, NSObject))]
    #[name = "NSMutableData"]
    pub(crate) struct NSMutableDataImpl;

    impl NSMutableDataImpl {
        #[unsafe(method_id(data))]
        fn data() -> Retained<NSMutableData> {
            make_mutable(Vec::new())
        }

        #[unsafe(method_id(dataWithBytes:length:))]
        fn data_with_bytes(bytes: *const c_void, length: NSUInteger) -> Retained<NSMutableData> {
            // SAFETY: the caller passes `length` readable bytes.
            make_mutable(unsafe { raw(bytes, length) }.to_vec())
        }

        #[unsafe(method_id(dataWithData:))]
        fn data_with_data(data: &NSData) -> Retained<NSMutableData> {
            make_mutable(bytes(data).to_vec())
        }

        #[unsafe(method_id(dataWithContentsOfFile:))]
        fn data_with_contents_of_file(path: &NSString) -> Option<Retained<NSMutableData>> {
            std::fs::read(path.to_string()).ok().map(make_mutable)
        }

        #[unsafe(method_id(dataWithContentsOfFile:options:error:))]
        fn data_with_contents_of_file_error(
            path: &NSString,
            _options: NSUInteger,
            error: *mut *mut NSError,
        ) -> Option<Retained<NSMutableData>> {
            // SAFETY: the caller passes null or room for an error.
            unsafe { read_path(&path.to_string(), error) }.map(make_mutable)
        }

        #[unsafe(method_id(dataWithContentsOfURL:))]
        fn data_with_contents_of_url(url: &NSURL) -> Option<Retained<NSMutableData>> {
            // SAFETY: no error out-parameter.
            unsafe { read_url(url, std::ptr::null_mut()) }.map(make_mutable)
        }

        #[unsafe(method_id(dataWithContentsOfURL:options:error:))]
        fn data_with_contents_of_url_error(
            url: &NSURL,
            _options: NSUInteger,
            error: *mut *mut NSError,
        ) -> Option<Retained<NSMutableData>> {
            // SAFETY: the caller passes null or room for an error.
            unsafe { read_url(url, error) }.map(make_mutable)
        }

        #[unsafe(method_id(dataWithBytesNoCopy:length:))]
        fn data_no_copy(bytes: NonNull<c_void>, length: NSUInteger) -> Retained<NSMutableData> {
            // SAFETY: the caller hands over `length` bytes from malloc.
            let data = make_mutable(unsafe { raw(bytes.as_ptr(), length) }.to_vec());
            // SAFETY: as above; freed once.
            unsafe { libc::free(bytes.as_ptr()) };
            data
        }

        #[unsafe(method_id(dataWithBytesNoCopy:length:freeWhenDone:))]
        fn data_no_copy_free(bytes: NonNull<c_void>, length: NSUInteger, free: bool) -> Retained<NSMutableData> {
            // SAFETY: the caller passes `length` readable bytes.
            let data = make_mutable(unsafe { raw(bytes.as_ptr(), length) }.to_vec());
            if free {
                // SAFETY: the caller asked for the malloc'd buffer to be freed.
                unsafe { libc::free(bytes.as_ptr()) };
            }
            data
        }

        #[unsafe(method_id(dataWithCapacity:))]
        fn data_with_capacity(_capacity: NSUInteger) -> Option<Retained<NSMutableData>> {
            Some(make_mutable(Vec::new()))
        }

        #[unsafe(method_id(dataWithLength:))]
        fn data_with_length(length: NSUInteger) -> Option<Retained<NSMutableData>> {
            Some(make_mutable(vec![0; length]))
        }

        #[unsafe(method_id(initWithCapacity:))]
        fn init_with_capacity(this: Allocated<Self>, capacity: NSUInteger) -> Option<Retained<Self>> {
            let bytes: Vec<u8> = Vec::with_capacity(capacity);
            // SAFETY: NSData's initializer; the vector's pointer is valid.
            unsafe { msg_send![this, initWithBytes: bytes.as_ptr().cast::<c_void>(), length: 0usize] }
        }

        #[unsafe(method_id(initWithLength:))]
        fn init_with_length(this: Allocated<Self>, length: NSUInteger) -> Option<Retained<Self>> {
            let bytes = vec![0u8; length];
            // SAFETY: as above.
            unsafe { msg_send![this, initWithBytes: bytes.as_ptr().cast::<c_void>(), length: length] }
        }

        #[unsafe(method(mutableBytes))]
        fn mutable_bytes(&self) -> *mut c_void {
            self.vec().as_mut_ptr().cast()
        }

        #[unsafe(method(setLength:))]
        fn set_length(&self, length: NSUInteger) {
            self.vec().resize(length, 0);
        }

        #[unsafe(method(increaseLengthBy:))]
        fn increase_length_by(&self, extra: NSUInteger) {
            let vec = self.vec();
            vec.resize(vec.len() + extra, 0);
        }

        #[unsafe(method(appendBytes:length:))]
        fn append_bytes(&self, bytes: NonNull<c_void>, length: NSUInteger) {
            // SAFETY: the caller passes `length` readable bytes, which may be
            // our own: copy them out first.
            let bytes = unsafe { raw(bytes.as_ptr(), length) }.to_vec();
            self.vec().extend_from_slice(&bytes);
        }

        #[unsafe(method(appendData:))]
        fn append_data(&self, data: &NSData) {
            let bytes = bytes(data).to_vec();
            self.vec().extend_from_slice(&bytes);
        }

        #[unsafe(method(replaceBytesInRange:withBytes:length:))]
        fn replace_bytes_length(&self, range: NSRange, bytes: *const c_void, length: NSUInteger) {
            // SAFETY: the caller passes `length` readable bytes.
            let bytes = unsafe { raw(bytes, length) }.to_vec();
            let vec = self.vec();
            let range = checked(range, vec.len(), "replaceBytesInRange:withBytes:length:");
            vec.splice(range, bytes);
        }

        #[unsafe(method(replaceBytesInRange:withBytes:))]
        fn replace_bytes(&self, range: NSRange, bytes: NonNull<c_void>) {
            // SAFETY: the caller passes as many bytes as the range covers.
            let bytes = unsafe { raw(bytes.as_ptr(), range.length) }.to_vec();
            let vec = self.vec();
            let range = checked(range, vec.len(), "replaceBytesInRange:withBytes:");
            vec[range].copy_from_slice(&bytes);
        }

        #[unsafe(method(resetBytesInRange:))]
        fn reset_bytes(&self, range: NSRange) {
            let vec = self.vec();
            let range = checked(range, vec.len(), "resetBytesInRange:");
            vec[range].fill(0);
        }

        #[unsafe(method(setData:))]
        fn set_data(&self, data: &NSData) {
            let bytes = bytes(data).to_vec();
            *self.vec() = bytes;
        }
    }
);

impl NSMutableDataImpl {
    /// The growable bytes of mutable data.
    #[allow(clippy::mut_from_ref)]
    fn vec(&self) -> &mut Vec<u8> {
        // SAFETY: NSMutableDataImpl instances are NSDataImpl instances.
        let data = unsafe { &*(self as *const Self).cast::<NSDataImpl>() };
        // SAFETY: mutable data isn't thread-safe, and each method finishes
        // with this reference before sending any message.
        match unsafe { data.storage_mut() } {
            Storage::Growable(vec) => vec,
            _ => unreachable!("mutable data grows"),
        }
    }
}

/// The bytes of any `NSData`, without copying.
pub fn bytes(data: &NSData) -> &[u8] {
    let object: &AnyObject = data.as_ref();
    let class: *const AnyClass = object.class();
    let ours = |shell: &'static sidestep_runtime::Class| {
        std::ptr::eq(class.cast::<u8>(), (shell as *const sidestep_runtime::Class).cast::<u8>())
    };
    if ours(&NSDATA) || ours(&NSMUTABLEDATA) {
        // SAFETY: an instance of this module's classes.
        return unsafe { &*(data as *const NSData).cast::<NSDataImpl>() }.contents();
    }
    // SAFETY: -bytes and -length describe the data's contents, alive as long
    // as the data is (and unmutated).
    unsafe {
        let ptr: *const c_void = msg_send![data, bytes];
        let len: NSUInteger = msg_send![data, length];
        raw(ptr, len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions() {
        assert_eq!(describe(&[1, 2, 3]), "{length = 3, bytes = 0x010203}");
        assert_eq!(describe(&[]), "{length = 0, bytes = 0x}");
        let long: Vec<u8> = (0..40).collect();
        assert_eq!(
            describe(&long),
            "{length = 40, bytes = 0x00010203 04050607 08090a0b 0c0d0e0f ... 20212223 24252627 }"
        );
    }
}
