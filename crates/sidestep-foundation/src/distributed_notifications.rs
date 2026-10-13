//! Per-user distributed notifications over Unix datagrams. Payloads are binary
//! property lists and deliveries run on the main run loop. No desktop service
//! or bus address is required, including in headless sessions.
use crate::thread::lock;
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSString};
use std::collections::VecDeque;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, mpsc};

crate::constant_string!(NSLocalNotificationCenterType = "NSLocalNotificationCenterType");
sidestep_runtime::static_class!(pub(crate) DISTRIBUTED, DISTRIBUTED_META = "NSDistributedNotificationCenter", || { let _ = Center::class(); });
#[derive(Clone)]
struct Event {
    name: String,
    object: Option<String>,
    info: Option<plist::Value>,
    immediate: bool,
}
struct Observer {
    observer: Weak<AnyObject>,
    address: usize,
    selector: Sel,
    name: Option<String>,
    object: Option<String>,
    behavior: usize,
    pending: VecDeque<Event>,
}
struct State {
    suspended: bool,
    observers: Vec<Observer>,
}
// Weak loads and runtime reference counts are thread safe. Observers are
// messaged only on the main run loop, outside this lock.
unsafe impl Send for State {}
struct Ivars {
    state: Mutex<State>,
    send: mpsc::Sender<Event>,
}
static DEFAULT: OnceLock<usize> = OnceLock::new();
define_class!(
    #[unsafe(super(NSNotificationCenter, NSObject))]
    #[name = "NSDistributedNotificationCenter"]
    #[ivars = Ivars]
    struct Center;
    impl Center {
        #[unsafe(method_id(defaultCenter))]
        fn default_center() -> Retained<Self> { default_center() }
        #[unsafe(method_id(notificationCenterForType:))]
        fn typed(_kind: &NSString) -> Retained<Self> { default_center() }
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let (send, recv) = mpsc::channel();
            let this = this.set_ivars(Ivars { state: Mutex::new(State { suspended: false, observers: Vec::new() }), send });
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            start(&this, recv); this
        }
        #[unsafe(method(addObserver:selector:name:object:))]
        fn add(&self, observer: &AnyObject, selector: Sel, name: Option<&NSString>, object: Option<&NSString>) { add(self, observer, selector, name, object, 2); }
        #[unsafe(method(addObserver:selector:name:object:suspensionBehavior:))]
        fn add_behavior(&self, observer: &AnyObject, selector: Sel, name: Option<&NSString>, object: Option<&NSString>, behavior: usize) { add(self, observer, selector, name, object, behavior); }
        #[unsafe(method(removeObserver:))]
        fn remove(&self, observer: &AnyObject) { remove(self, observer, None, None); }
        #[unsafe(method(removeObserver:name:object:))]
        fn remove_named(&self, observer: &AnyObject, name: Option<&NSString>, object: Option<&NSString>) { remove(self, observer, name, object); }
        #[unsafe(method(postNotificationName:object:))]
        fn post(&self, name: &NSString, object: Option<&NSString>) { post(self, name, object, None, false); }
        #[unsafe(method(postNotificationName:object:userInfo:))]
        fn post_info(&self, name: &NSString, object: Option<&NSString>, info: Option<&AnyObject>) { post(self, name, object, info, false); }
        #[unsafe(method(postNotificationName:object:userInfo:deliverImmediately:))]
        fn post_now(&self, name: &NSString, object: Option<&NSString>, info: Option<&AnyObject>, immediate: bool) { post(self, name, object, info, immediate); }
        #[unsafe(method(postNotificationName:object:userInfo:options:))]
        fn post_options(&self, name: &NSString, object: Option<&NSString>, info: Option<&AnyObject>, options: usize) { post(self, name, object, info, options & 1 != 0); }
        #[unsafe(method(postNotification:))]
        fn post_notification(&self, notification: &NSNotification) {
            let object = notification.object().and_then(|o| o.downcast::<NSString>().ok());
            let info = notification.userInfo(); post(self, &notification.name(), object.as_deref(), info.as_deref().map(|d| d as &AnyObject), false);
        }
        #[unsafe(method(suspended))]
        fn suspended(&self) -> bool { lock(&self.ivars().state).suspended }
        #[unsafe(method(setSuspended:))]
        fn set_suspended(&self, suspended: bool) {
            let deliveries = { let mut s = lock(&self.ivars().state); s.suspended = suspended; let mut out = Vec::new(); if !suspended { for o in &mut s.observers { if let Some(target) = o.observer.load() { for e in o.pending.drain(..) { out.push((target.clone(), o.selector, e)); } } } } out };
            for (target, selector, event) in deliveries { deliver(&target, selector, &event); }
        }
    }
    unsafe impl NSObjectProtocol for Center {}
);
fn default_center() -> Retained<Center> {
    let ptr = DEFAULT.get_or_init(|| {
        let center: Retained<Center> = unsafe { msg_send![Center::alloc(), init] };
        Retained::into_raw(center) as usize
    });
    unsafe { Retained::retain(*ptr as *mut Center).unwrap() }
}
fn add(
    center: &Center,
    observer: &AnyObject,
    selector: Sel,
    name: Option<&NSString>,
    object: Option<&NSString>,
    behavior: usize,
) {
    lock(&center.ivars().state).observers.push(Observer {
        observer: Weak::new(observer),
        address: observer as *const AnyObject as usize,
        selector,
        name: name.map(ToString::to_string),
        object: object.map(ToString::to_string),
        behavior,
        pending: VecDeque::new(),
    });
}
fn remove(center: &Center, observer: &AnyObject, name: Option<&NSString>, object: Option<&NSString>) {
    let name = name.map(ToString::to_string);
    let object = object.map(ToString::to_string);
    lock(&center.ivars().state).observers.retain(|o| {
        o.address != observer as *const AnyObject as usize
            || name.as_ref().is_some_and(|n| o.name.as_ref() != Some(n))
            || object.as_ref().is_some_and(|v| o.object.as_ref() != Some(v))
    });
}
fn post(center: &Center, name: &NSString, object: Option<&NSString>, info: Option<&AnyObject>, immediate: bool) {
    let info = info.and_then(crate::plist::from_object);
    let _ = center.ivars().send.send(Event {
        name: name.to_string(),
        object: object.map(ToString::to_string),
        info,
        immediate,
    });
}
/// Deliver `name` to this process's observers of the default center, as
/// macOS's system delivers what the desktop says
/// (`AppleInterfaceThemeChangedNotification`). Every program hears the
/// desktop itself, so this isn't sent to the others. On the main thread.
pub fn post_from_desktop(name: &str) {
    received(&default_center(), Event { name: name.to_string(), object: None, info: None, immediate: false });
}
fn deliver(target: &AnyObject, selector: Sel, event: &Event) {
    let name = NSString::from_str(&event.name);
    let object = event.object.as_ref().map(|s| NSString::from_str(s));
    let info = event.info.as_ref().and_then(crate::plist::to_object);
    let notification =
        crate::notification::notification_with(&name, object.as_deref().map(|s| s as &AnyObject), info.as_deref());
    // Selector observers accept exactly one NSNotification, as addObserver requires.
    unsafe { crate::perform::send_object(target, selector, Some(notification.as_ref())) };
}
fn received(center: &Center, event: Event) {
    let deliveries = {
        let mut s = lock(&center.ivars().state);
        let suspended = s.suspended;
        let mut out = Vec::new();
        s.observers.retain(|o| o.observer.load().is_some());
        for o in &mut s.observers {
            if o.name.as_ref().is_some_and(|n| n != &event.name)
                || o.object.as_ref().is_some_and(|v| event.object.as_ref() != Some(v))
            {
                continue;
            }
            if suspended && !event.immediate && o.behavior != 4 {
                match o.behavior {
                    1 => {}
                    2 => {
                        o.pending.retain(|e| e.name != event.name || e.object != event.object);
                        o.pending.push_back(event.clone());
                    }
                    _ => o.pending.push_back(event.clone()),
                }
            } else if let Some(target) = o.observer.load() {
                out.push((target, o.selector));
            }
        }
        out
    };
    for (target, selector) in deliveries {
        deliver(&target, selector, &event);
    }
}
fn encode(event: &Event) -> Option<Vec<u8>> {
    let mut d = plist::Dictionary::new();
    d.insert("name".into(), event.name.clone().into());
    d.insert("immediate".into(), event.immediate.into());
    if let Some(s) = &event.object {
        d.insert("object".into(), s.clone().into());
    }
    if let Some(info) = &event.info {
        d.insert("info".into(), info.clone());
    }
    crate::plist::write_binary(&plist::Value::Dictionary(d)).filter(|b| b.len() <= 60_000)
}
fn decode(bytes: &[u8]) -> Option<Event> {
    let (value, _) = crate::plist::parse(bytes)?;
    let d = value.as_dictionary()?;
    Some(Event {
        name: d.get("name")?.as_string()?.into(),
        object: d.get("object").and_then(|s| s.as_string()).map(str::to_owned),
        info: d.get("info").cloned(),
        immediate: d.get("immediate").and_then(|v| v.as_boolean()).unwrap_or(false),
    })
}
fn directory() -> std::io::Result<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("sidestep-notifications-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    Ok(dir)
}
// The reader holds a weak reference so a non-default center can be released.
struct CenterWeak(Weak<Center>);
unsafe impl Send for CenterWeak {}
impl CenterWeak {
    fn enqueue(&self, event: Event) {
        if let Some(center) = self.0.load() {
            let address = Retained::into_raw(center) as usize;
            crate::runloop::core::on_owner(crate::runloop::core::main_shared(), move || {
                let center = unsafe { Retained::from_raw(address as *mut Center).unwrap() };
                received(&center, event);
            });
        }
    }
}
fn start(center: &Center, events: mpsc::Receiver<Event>) {
    let weak = CenterWeak(Weak::new(center));
    let endpoint = directory().and_then(|dir| {
        let path = dir.join(format!("{}-{}.sock", std::process::id(), objc2_foundation::NSUUID::UUID().UUIDString()));
        let socket = UnixDatagram::bind(&path)?;
        socket.set_read_timeout(Some(std::time::Duration::from_millis(100)))?;
        Ok((dir, path, socket))
    });
    let Ok((dir, path, socket)) = endpoint else {
        let _ = std::thread::Builder::new().name("NSNotification.local".into()).spawn(move || {
            for event in events {
                weak.enqueue(event);
            }
        });
        return;
    };
    let reader_weak = CenterWeak(Weak::new(center));
    let reader_path = path.clone();
    let _ = std::thread::Builder::new().name("NSNotification.recv".into()).spawn(move || {
        let mut bytes = [0; 65536];
        loop {
            if reader_weak.0.load().is_none() {
                break;
            }
            if let Ok(n) = socket.recv(&mut bytes)
                && let Some(event) = decode(&bytes[..n])
            {
                reader_weak.enqueue(event);
            }
        }
        let _ = std::fs::remove_file(reader_path);
    });
    let _ = std::thread::Builder::new().name("NSNotification.send".into()).spawn(move || {
        let socket = UnixDatagram::unbound().ok();
        if let Some(s) = &socket {
            let _ = s.set_nonblocking(true);
        }
        for event in events {
            weak.enqueue(event.clone());
            let Some(bytes) = encode(&event) else { continue };
            if let (Some(socket), Ok(entries)) = (&socket, std::fs::read_dir(&dir)) {
                for entry in entries.flatten() {
                    let peer = entry.path();
                    if peer == path || peer.extension().is_none_or(|e| e != "sock") {
                        continue;
                    }
                    if socket.send_to(&bytes, &peer).is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused) {
                        let _ = std::fs::remove_file(peer);
                    }
                }
            }
        }
    });
}
