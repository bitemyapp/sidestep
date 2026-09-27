//! `CGDataProvider` and `CGDataConsumer`: where images' bytes come from
//! and where encoded files go.
//!
//! A provider hands out its bytes whole ([`CGDataProviderImpl::bytes`]):
//! a file's are read when it's made (a missing file makes none, as in
//! CoreGraphics), a `CFData`'s and callbacks' the first time they're asked
//! for, then kept (a provider's data doesn't change). Memory the program
//! lends (`CGDataProviderCreateWithData`) is copied the first time it's
//! read (so the copy can be shared, as other providers' bytes are), and
//! its release callback runs when the provider goes, as do the callbacks'
//! release.

use std::ffi::{c_char, c_uint, c_void};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::{CFData, CFMutableData, CFTypeID, CFURL};
use objc2_core_graphics::{CGDataConsumer, CGDataConsumerCallbacks, CGDataProvider, CGDataProviderReleaseDataCallback};
use objc2_foundation::NSString;

/// `CGDataProviderSequentialCallbacks`, with `off_t` as Linux has it.
#[repr(C)]
pub struct SequentialCallbacks {
    pub version: c_uint,
    pub get_bytes: Option<unsafe extern "C-unwind" fn(*mut c_void, NonNull<c_void>, usize) -> usize>,
    pub skip_forward: Option<unsafe extern "C-unwind" fn(*mut c_void, i64) -> i64>,
    pub rewind: Option<unsafe extern "C-unwind" fn(*mut c_void)>,
    pub release_info: Option<unsafe extern "C-unwind" fn(*mut c_void)>,
}

/// `CGDataProviderDirectCallbacks`, with `off_t` as Linux has it.
#[repr(C)]
pub struct DirectCallbacks {
    pub version: c_uint,
    pub get_byte_pointer: Option<unsafe extern "C-unwind" fn(*mut c_void) -> *const c_void>,
    pub release_byte_pointer: Option<unsafe extern "C-unwind" fn(*mut c_void, NonNull<c_void>)>,
    pub get_bytes_at_position: Option<unsafe extern "C-unwind" fn(*mut c_void, NonNull<c_void>, i64, usize) -> usize>,
    pub release_info: Option<unsafe extern "C-unwind" fn(*mut c_void)>,
}

/// Where a provider's bytes come from.
enum Source {
    /// Bytes of its own (a file's).
    Bytes(Arc<[u8]>),
    /// A `CFData` (an `NSData`).
    Data(Retained<AnyObject>),
    /// Memory the program lends, and what to call when the provider goes.
    Lent {
        ptr: *const u8,
        len: usize,
        release: CGDataProviderReleaseDataCallback,
    },
    Direct {
        size: usize,
        callbacks: DirectCallbacks,
    },
    Sequential(SequentialCallbacks),
    /// An encoded file's pixels, as 8-bit RGBA, straight, turned upright
    /// by the file's orientation or not: decoded the first time they're
    /// asked for.
    Decoded(Arc<[u8]>, bool),
}

pub(crate) struct ProviderIvars {
    source: Source,
    info: *mut c_void,
    /// The bytes, once asked for (for sources that aren't bytes already).
    cache: OnceLock<Option<Arc<[u8]>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a provider's data
    // doesn't change, and what's read of it is kept behind a OnceLock.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGDataProvider"]
    #[ivars = ProviderIvars]
    pub(crate) struct CGDataProviderImpl;

    impl CGDataProviderImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            super::description("CGDataProvider", self, "")
        }
    }

    unsafe impl NSObjectProtocol for CGDataProviderImpl {}
);

impl Drop for CGDataProviderImpl {
    fn drop(&mut self) {
        let info = self.ivars().info;
        match &self.ivars().source {
            Source::Lent { ptr, len, release: Some(release) } => {
                if let Some(ptr) = NonNull::new(ptr.cast_mut().cast()) {
                    // SAFETY: the program's callback, with what it lent.
                    unsafe { release(info, ptr, *len) };
                }
            }
            Source::Direct { callbacks: DirectCallbacks { release_info: Some(f), .. }, .. }
            | Source::Sequential(SequentialCallbacks { release_info: Some(f), .. }) => {
                // SAFETY: the program's callback, with its info.
                unsafe { f(info) };
            }
            _ => {}
        }
    }
}

impl CGDataProviderImpl {
    /// All the bytes.
    pub(crate) fn bytes(&self) -> Option<Arc<[u8]>> {
        match &self.ivars().source {
            Source::Bytes(b) => Some(b.clone()),
            _ => self.ivars().cache.get_or_init(|| self.read()).clone(),
        }
    }

    /// How many bytes there are, when that's known without reading them.
    pub(crate) fn known_len(&self) -> Option<usize> {
        match &self.ivars().source {
            Source::Bytes(b) => Some(b.len()),
            // SAFETY: an NSData answers -length.
            Source::Data(d) => Some(unsafe { msg_send![&**d, length] }),
            Source::Lent { len, .. } => Some(*len),
            Source::Direct { size, .. } => Some(*size),
            Source::Sequential(_) | Source::Decoded(..) => None,
        }
    }

    fn read(&self) -> Option<Arc<[u8]>> {
        let info = self.ivars().info;
        match &self.ivars().source {
            Source::Bytes(b) => Some(b.clone()),
            Source::Decoded(file, upright) => crate::codec::decode_straight(file, *upright).map(|d| Arc::from(d.rgba)),
            Source::Data(d) => Some(crate::image_rep::data_bytes(d).unwrap_or_else(|| Arc::from(&[][..]))),
            Source::Lent { ptr, len, .. } => {
                if ptr.is_null() {
                    return Some(Arc::from(&[][..]));
                }
                // SAFETY: the program lent `len` bytes at `ptr` for the
                // provider's life.
                Some(Arc::from(unsafe { std::slice::from_raw_parts(*ptr, *len) }))
            }
            Source::Direct { size, callbacks } => {
                if let Some(get) = callbacks.get_byte_pointer {
                    // SAFETY: the program's callbacks, with their info.
                    let p = unsafe { get(info) };
                    if !p.is_null() {
                        // SAFETY: the pointer holds `size` bytes until
                        // released.
                        let bytes = Arc::from(unsafe { std::slice::from_raw_parts(p.cast::<u8>(), *size) });
                        if let (Some(release), Some(p)) = (callbacks.release_byte_pointer, NonNull::new(p.cast_mut())) {
                            // SAFETY: as above.
                            unsafe { release(info, p) };
                        }
                        return Some(bytes);
                    }
                }
                let get = callbacks.get_bytes_at_position?;
                let mut out = vec![0u8; *size];
                let mut at = 0;
                while at < out.len() {
                    // SAFETY: the buffer has room for what's asked.
                    let n = unsafe { get(info, NonNull::from(&mut out[at]).cast(), at as i64, out.len() - at) };
                    if n == 0 {
                        break;
                    }
                    at += n.min(out.len() - at);
                }
                out.truncate(at);
                Some(Arc::from(out))
            }
            Source::Sequential(callbacks) => {
                let get = callbacks.get_bytes?;
                if let Some(rewind) = callbacks.rewind {
                    // SAFETY: the program's callback.
                    unsafe { rewind(info) };
                }
                let mut out = Vec::new();
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    // SAFETY: the chunk has room for what's asked.
                    let n = unsafe { get(info, NonNull::from(&mut chunk[0]).cast(), chunk.len()) };
                    if n == 0 {
                        break;
                    }
                    out.extend_from_slice(&chunk[..n.min(chunk.len())]);
                }
                Some(Arc::from(out))
            }
        }
    }
}

pub(crate) fn provider_imp(p: &CGDataProvider) -> &CGDataProviderImpl {
    // SAFETY: every CGDataProvider is a CGDataProviderImpl.
    unsafe { &*(p as *const CGDataProvider).cast::<CGDataProviderImpl>() }
}

fn make(source: Source, info: *mut c_void) -> Retained<CGDataProviderImpl> {
    let this = CGDataProviderImpl::alloc().set_ivars(ProviderIvars { source, info, cache: OnceLock::new() });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A provider of `bytes`.
pub(crate) fn of_bytes(bytes: Arc<[u8]>) -> Retained<CGDataProviderImpl> {
    make(Source::Bytes(bytes), std::ptr::null_mut())
}

/// A provider of an encoded file's pixels (8-bit RGBA, straight, upright
/// or as stored), decoded when they're first asked for.
pub(crate) fn of_file(file: Arc<[u8]>, upright: bool) -> Retained<CGDataProviderImpl> {
    make(Source::Decoded(file, upright), std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGDataProviderGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_DATA_PROVIDER
}

/// # Safety
///
/// `callbacks` points at callbacks valid for `info`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGDataProviderCreateSequential(
    info: *mut c_void,
    callbacks: *const SequentialCallbacks,
) -> Option<NonNull<CGDataProvider>> {
    if callbacks.is_null() {
        return None;
    }
    // SAFETY: as the caller promises.
    let c = unsafe { &*callbacks };
    let callbacks = SequentialCallbacks {
        version: c.version,
        get_bytes: c.get_bytes,
        skip_forward: c.skip_forward,
        rewind: c.rewind,
        release_info: c.release_info,
    };
    Some(super::owned(make(Source::Sequential(callbacks), info)))
}

/// # Safety
///
/// `callbacks` points at callbacks valid for `info`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGDataProviderCreateDirect(
    info: *mut c_void,
    size: i64,
    callbacks: *const DirectCallbacks,
) -> Option<NonNull<CGDataProvider>> {
    if callbacks.is_null() || size < 0 {
        return None;
    }
    // SAFETY: as the caller promises.
    let c = unsafe { &*callbacks };
    let callbacks = DirectCallbacks {
        version: c.version,
        get_byte_pointer: c.get_byte_pointer,
        release_byte_pointer: c.release_byte_pointer,
        get_bytes_at_position: c.get_bytes_at_position,
        release_info: c.release_info,
    };
    Some(super::owned(make(Source::Direct { size: size as usize, callbacks }, info)))
}

/// # Safety
///
/// `data` holds `size` bytes until `release_data` is called.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGDataProviderCreateWithData(
    info: *mut c_void,
    data: *const c_void,
    size: usize,
    release_data: CGDataProviderReleaseDataCallback,
) -> Option<NonNull<CGDataProvider>> {
    Some(super::owned(make(Source::Lent { ptr: data.cast(), len: size, release: release_data }, info)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGDataProviderCreateWithCFData(data: Option<&CFData>) -> Option<NonNull<CGDataProvider>> {
    // SAFETY: a CFData is an NSData here.
    let data = unsafe { &*(data? as *const CFData).cast::<AnyObject>() };
    // No bytes make no provider, as in CoreGraphics.
    // SAFETY: an NSData answers -length.
    let len: usize = unsafe { msg_send![data, length] };
    if len == 0 {
        return None;
    }
    Some(super::owned(make(Source::Data(data.retain()), std::ptr::null_mut())))
}

/// The file path of a file URL.
pub(crate) fn url_path(url: &CFURL) -> Option<String> {
    // SAFETY: a CFURL is an NSURL here.
    let url = unsafe { &*(url as *const CFURL).cast::<AnyObject>() };
    crate::image_rep::url_path(url).map(|p| p.to_string())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGDataProviderCreateWithURL(url: Option<&CFURL>) -> Option<NonNull<CGDataProvider>> {
    let bytes = crate::image_rep::read_file(&url_path(url?)?)?;
    Some(super::owned(of_bytes(bytes)))
}

/// # Safety
///
/// `filename` is a NUL-terminated path.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGDataProviderCreateWithFilename(
    filename: *const c_char,
) -> Option<NonNull<CGDataProvider>> {
    if filename.is_null() {
        return None;
    }
    // SAFETY: as the caller promises.
    let path = unsafe { std::ffi::CStr::from_ptr(filename) }.to_str().ok()?;
    let bytes = crate::image_rep::read_file(path)?;
    Some(super::owned(of_bytes(bytes)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGDataProviderCopyData(provider: Option<&CGDataProvider>) -> Option<NonNull<CFData>> {
    let bytes = provider_imp(provider?).bytes()?;
    crate::image_rep::make_data(&bytes).map(super::owned)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGDataProviderGetInfo(provider: Option<&CGDataProvider>) -> *mut c_void {
    provider.map_or(std::ptr::null_mut(), |p| provider_imp(p).ivars().info)
}

// Consumers.

/// Where a consumer's bytes go.
enum Sink {
    Callbacks {
        put: Option<unsafe extern "C-unwind" fn(*mut c_void, NonNull<c_void>, usize) -> usize>,
        release: Option<unsafe extern "C-unwind" fn(*mut c_void)>,
    },
    /// A `CFMutableData` (an `NSMutableData`).
    Data(Retained<AnyObject>),
    File(Mutex<std::fs::File>),
}

pub(crate) struct ConsumerIvars {
    sink: Sink,
    info: *mut c_void,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a consumer is used
    // by one thread at a time.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGDataConsumer"]
    #[ivars = ConsumerIvars]
    pub(crate) struct CGDataConsumerImpl;

    unsafe impl NSObjectProtocol for CGDataConsumerImpl {}
);

impl Drop for CGDataConsumerImpl {
    fn drop(&mut self) {
        if let Sink::Callbacks { release: Some(release), .. } = &self.ivars().sink {
            // SAFETY: the program's callback, with its info.
            unsafe { release(self.ivars().info) };
        }
    }
}

impl CGDataConsumerImpl {
    /// Hand `bytes` on; how many were taken.
    pub(crate) fn put(&self, bytes: &[u8]) -> usize {
        if bytes.is_empty() {
            return 0;
        }
        match &self.ivars().sink {
            Sink::Callbacks { put: Some(put), .. } => {
                let mut at = 0;
                while at < bytes.len() {
                    // SAFETY: the program's callback, reading what's left.
                    let n = unsafe { put(self.ivars().info, NonNull::from(&bytes[at]).cast(), bytes.len() - at) };
                    if n == 0 {
                        break;
                    }
                    at += n.min(bytes.len() - at);
                }
                at
            }
            Sink::Callbacks { put: None, .. } => 0,
            Sink::Data(d) => {
                // SAFETY: an NSMutableData answers -appendBytes:length:.
                let _: () =
                    unsafe { msg_send![&**d, appendBytes: bytes.as_ptr().cast::<c_void>(), length: bytes.len()] };
                bytes.len()
            }
            Sink::File(f) => {
                use std::io::Write as _;
                let mut f = f.lock().unwrap_or_else(|e| e.into_inner());
                if f.write_all(bytes).is_ok() { bytes.len() } else { 0 }
            }
        }
    }
}

pub(crate) fn consumer_imp(c: &CGDataConsumer) -> &CGDataConsumerImpl {
    // SAFETY: every CGDataConsumer is a CGDataConsumerImpl.
    unsafe { &*(c as *const CGDataConsumer).cast::<CGDataConsumerImpl>() }
}

fn make_consumer(sink: Sink, info: *mut c_void) -> Retained<CGDataConsumerImpl> {
    let this = CGDataConsumerImpl::alloc().set_ivars(ConsumerIvars { sink, info });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGDataConsumerGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_DATA_CONSUMER
}

/// # Safety
///
/// `cbks` points at callbacks valid for `info`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGDataConsumerCreate(
    info: *mut c_void,
    cbks: *const CGDataConsumerCallbacks,
) -> Option<NonNull<CGDataConsumer>> {
    if cbks.is_null() {
        return None;
    }
    // SAFETY: as the caller promises.
    let c = unsafe { &*cbks };
    Some(super::owned(make_consumer(Sink::Callbacks { put: c.putBytes, release: c.releaseConsumer }, info)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGDataConsumerCreateWithURL(url: Option<&CFURL>) -> Option<NonNull<CGDataConsumer>> {
    let file = std::fs::File::create(url_path(url?)?).ok()?;
    Some(super::owned(make_consumer(Sink::File(Mutex::new(file)), std::ptr::null_mut())))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGDataConsumerCreateWithCFData(
    data: Option<&CFMutableData>,
) -> Option<NonNull<CGDataConsumer>> {
    // SAFETY: a CFMutableData is an NSMutableData here.
    let data = unsafe { &*(data? as *const CFMutableData).cast::<AnyObject>() };
    Some(super::owned(make_consumer(Sink::Data(data.retain()), std::ptr::null_mut())))
}
