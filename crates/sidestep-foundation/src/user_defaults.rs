//! `NSUserDefaults`: layered preference domains, kept in memory and
//! written behind.
//!
//! A lookup searches, in order: the argument domain (`-key value` pairs
//! from the command line), the instance's own persistent domain (the
//! application's, named by its bundle identifier or process name, or a
//! suite's), suites added with `addSuiteNamed:`, `NSGlobalDomain`, and
//! the registration domain. Domains are shared by every instance in the
//! process, as on macOS; so is the registration domain.
//!
//! A persistent domain named `N` lives in `$XDG_CONFIG_HOME/N/defaults.plist`
//! as an XML property list, read on first use. Changes go to memory and a
//! background thread writes the changed domains (a temporary file,
//! `sync_all`, then a rename), shortly after; `synchronize` writes them
//! at once, and so does an `atexit` hook. Each change posts
//! `NSUserDefaultsDidChangeNotification` on the changing thread.
//!
//! A domain's values are shared (`Arc`) and copied on write, so a write
//! holds the store's lock only to take its snapshot: readers on other
//! threads, the main thread among them, never wait for a domain to be
//! serialized or written. Writes are serialized from snapshot to rename,
//! so the exit hook, finding nothing left to write, knows every change is
//! on disk rather than on its way there.
//!
//! Values are held as `plist::Value`s, so the typed accessors work
//! without Foundation's collections; `objectForKey:` hands out numbers
//! and arrays only once those exist. The typed getters coerce as macOS
//! does (`conformance/tests/defaults.rs`): strings that are whole
//! integers for `integerForKey:`, decimal numbers for `doubleForKey:`,
//! and "YES", "true" or "1" for `boolForKey:`. File URLs are stored as
//! paths with the home directory abbreviated to `~`, other URLs as keyed
//! archives.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::time::Duration;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSData, NSDictionary, NSInteger, NSString, NSURL};
use plist::Value;

use crate::runloop::modes::{constant, exported_strings};

sidestep_runtime::static_class!(pub(crate) NSUSERDEFAULTS, NSUSERDEFAULTS_META = "NSUserDefaults", || {
    let _ = NSUserDefaultsImpl::class();
    crate::perform::install();
});

exported_strings! {
    NSGlobalDomain, GLOBAL_DOMAIN = "NSGlobalDomain";
    NSArgumentDomain, ARGUMENT_DOMAIN = "NSArgumentDomain";
    NSRegistrationDomain, REGISTRATION_DOMAIN = "NSRegistrationDomain";
    NSUserDefaultsDidChangeNotification, DID_CHANGE = "NSUserDefaultsDidChangeNotification";
}

const GLOBAL: &str = "NSGlobalDomain";
const ARGUMENT: &str = "NSArgumentDomain";
const REGISTRATION: &str = "NSRegistrationDomain";

/// Every domain in the process.
struct Store {
    persistent: HashMap<String, Persistent>,
    volatile: HashMap<String, plist::Dictionary>,
}

struct Persistent {
    /// Shared with a write in progress; copied if changed meanwhile.
    values: Arc<plist::Dictionary>,
    dirty: bool,
    /// Removed: its file goes at the next write.
    removed: bool,
}

fn store() -> &'static RwLock<Store> {
    static STORE: OnceLock<RwLock<Store>> = OnceLock::new();
    STORE.get_or_init(|| {
        let mut volatile = HashMap::new();
        volatile.insert(ARGUMENT.to_string(), argument_domain(std::env::args().skip(1)));
        volatile.insert(REGISTRATION.to_string(), plist::Dictionary::new());
        RwLock::new(Store { persistent: HashMap::new(), volatile })
    })
}

/// The `-key value` pairs of a command line. Values that look like
/// property lists (`(...)`, `{...}`, `<...>`, quoted) are parsed.
pub(crate) fn argument_domain(args: impl Iterator<Item = String>) -> plist::Dictionary {
    let mut out = plist::Dictionary::new();
    let args: Vec<String> = args.collect();
    let mut i = 0;
    while i + 1 < args.len() {
        let arg = &args[i];
        if let Some(key) = arg.strip_prefix('-').filter(|k| !k.is_empty() && !k.starts_with('-')) {
            let text = &args[i + 1];
            let parsed = text
                .starts_with(['(', '{', '<', '"'])
                .then(|| crate::plist::parse(text.as_bytes()).map(|(v, _)| v))
                .flatten();
            out.insert(key.to_string(), parsed.unwrap_or_else(|| Value::String(text.clone())));
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

fn domain_file(name: &str) -> PathBuf {
    crate::xdg::config_home().join(name).join("defaults.plist")
}

/// Load a persistent domain from disk if it isn't in memory yet.
fn ensure_loaded(name: &str) {
    if crate::thread::lock_read(store()).persistent.contains_key(name) {
        return;
    }
    let values = std::fs::read(domain_file(name))
        .ok()
        .and_then(|bytes| crate::plist::parse(&bytes))
        .and_then(|(value, _)| value.into_dictionary())
        .unwrap_or_default();
    crate::thread::lock_write(store()).persistent.entry(name.to_string()).or_insert(Persistent {
        values: Arc::new(values),
        dirty: false,
        removed: false,
    });
}

/// Held by a write of the domain files from its snapshot to its last
/// rename.
static FLUSH: Mutex<()> = Mutex::new(());

/// Write the changed domains now.
pub(crate) fn flush() {
    let _flush = crate::thread::lock(&FLUSH);
    flush_from(crate::thread::lock_write(store()));
}

/// Write the changed domains from a locked store, holding [`FLUSH`]. The
/// store's lock is held only to take the snapshot: the values are
/// serialized and written after it is released.
fn flush_from(mut store: std::sync::RwLockWriteGuard<'_, Store>) {
    let pending: Vec<(String, Option<Arc<plist::Dictionary>>)> = store
        .persistent
        .iter_mut()
        .filter(|(_, d)| d.dirty)
        .map(|(name, d)| {
            d.dirty = false;
            (name.clone(), (!d.removed).then(|| d.values.clone()))
        })
        .collect();
    drop(store);
    for (name, values) in pending {
        let file = domain_file(&name);
        match values {
            Some(values) => {
                let bytes = crate::plist::write_xml_dictionary(&values);
                drop(values);
                if let Some(dir) = file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = crate::data::write(&file.to_string_lossy(), &bytes, 1);
            }
            None => {
                let _ = std::fs::remove_file(&file);
            }
        }
    }
}

/// Wake the writer, starting it (and the exit hook) on first use.
fn schedule_write() {
    struct Writer {
        pending: Mutex<bool>,
        ready: Condvar,
    }
    static WRITER: OnceLock<&'static Writer> = OnceLock::new();
    let writer = *WRITER.get_or_init(|| {
        let writer: &'static Writer = Box::leak(Box::new(Writer { pending: Mutex::new(false), ready: Condvar::new() }));
        std::thread::Builder::new()
            .name("NSUserDefaults writer".into())
            .spawn(move || {
                loop {
                    let mut pending = crate::thread::lock(&writer.pending);
                    while !*pending {
                        pending = writer.ready.wait(pending).unwrap_or_else(|e| e.into_inner());
                    }
                    *pending = false;
                    drop(pending);
                    // Let a burst of changes land in one write.
                    std::thread::sleep(Duration::from_millis(50));
                    flush();
                }
            })
            .expect("the defaults writer thread");
        extern "C" fn at_exit() {
            // Let a write in progress finish (the writer thread goes on
            // running while exit handlers run), then write what is left. A
            // thread may be stuck holding a lock at exit: wait a while for
            // each, but never hang the exit.
            let _ = std::panic::catch_unwind(|| {
                let Some(_flush) = (0..2000).find_map(|_| match FLUSH.try_lock() {
                    Ok(guard) => Some(guard),
                    Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
                    Err(std::sync::TryLockError::WouldBlock) => {
                        std::thread::sleep(Duration::from_millis(1));
                        None
                    }
                }) else {
                    return;
                };
                for _ in 0..200 {
                    match store().try_write() {
                        Ok(store) => return flush_from(store),
                        Err(std::sync::TryLockError::Poisoned(e)) => return flush_from(e.into_inner()),
                        Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
                    }
                }
            });
        }
        // SAFETY: registering a function that doesn't unwind.
        unsafe { libc::atexit(at_exit) };
        writer
    });
    *crate::thread::lock(&writer.pending) = true;
    writer.ready.notify_one();
}

pub(crate) struct DefaultsIvars {
    /// The persistent domain this instance reads and writes.
    domain: String,
    suites: Mutex<Vec<String>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSUserDefaults"]
    #[ivars = DefaultsIvars]
    pub(crate) struct NSUserDefaultsImpl;

    impl NSUserDefaultsImpl {
        #[unsafe(method_id(standardUserDefaults))]
        fn standard_user_defaults() -> Retained<Self> {
            standard()
        }

        #[unsafe(method(resetStandardUserDefaults))]
        fn reset_standard_user_defaults() {
            flush();
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_domain(this, app_domain())
        }

        #[unsafe(method_id(initWithSuiteName:))]
        fn init_with_suite_name(this: Allocated<Self>, suite: Option<&NSString>) -> Option<Retained<Self>> {
            init_suite(this, suite)
        }

        #[unsafe(method_id(objectForKey:))]
        fn object_for_key(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            self.value(&key.to_string()).and_then(|v| crate::plist::to_object(&v))
        }

        #[unsafe(method(setObject:forKey:))]
        fn set_object(&self, object: Option<&AnyObject>, key: &NSString) {
            let value = object.map(|o| {
                crate::plist::from_object(o).unwrap_or_else(|| {
                    panic!("NSUserDefaults: attempt to insert non-property list object for key {key}")
                })
            });
            self.set(&key.to_string(), value);
        }

        #[unsafe(method(removeObjectForKey:))]
        fn remove_object(&self, key: &NSString) {
            self.set(&key.to_string(), None);
        }

        #[unsafe(method_id(stringForKey:))]
        fn string_for_key(&self, key: &NSString) -> Option<Retained<NSString>> {
            self.value(&key.to_string()).and_then(|v| string_value(&v)).map(|s| NSString::from_str(&s))
        }

        #[unsafe(method_id(dictionaryForKey:))]
        fn dictionary_for_key(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            match self.value(&key.to_string()) {
                Some(v @ Value::Dictionary(_)) => crate::plist::to_object(&v),
                _ => None,
            }
        }

        #[unsafe(method_id(dataForKey:))]
        fn data_for_key(&self, key: &NSString) -> Option<Retained<NSData>> {
            match self.value(&key.to_string()) {
                Some(Value::Data(bytes)) => Some(NSData::from_vec(bytes)),
                _ => None,
            }
        }

        #[cfg(feature = "collections")]
        #[unsafe(method_id(arrayForKey:))]
        fn array_for_key(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            match self.value(&key.to_string()) {
                Some(v @ Value::Array(_)) => crate::plist::to_object(&v),
                _ => None,
            }
        }

        #[cfg(feature = "collections")]
        #[unsafe(method_id(stringArrayForKey:))]
        fn string_array_for_key(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            match self.value(&key.to_string()) {
                Some(v @ Value::Array(_)) if v.as_array().is_some_and(|a| a.iter().all(|i| i.as_string().is_some())) => {
                    crate::plist::to_object(&v)
                }
                _ => None,
            }
        }

        #[unsafe(method(integerForKey:))]
        fn integer_for_key(&self, key: &NSString) -> NSInteger {
            self.value(&key.to_string()).map_or(0, |v| integer_value(&v))
        }

        #[unsafe(method(floatForKey:))]
        fn float_for_key(&self, key: &NSString) -> f32 {
            self.value(&key.to_string()).map_or(0.0, |v| double_value(&v)) as f32
        }

        #[unsafe(method(doubleForKey:))]
        fn double_for_key(&self, key: &NSString) -> f64 {
            self.value(&key.to_string()).map_or(0.0, |v| double_value(&v))
        }

        #[unsafe(method(boolForKey:))]
        fn bool_for_key(&self, key: &NSString) -> bool {
            self.value(&key.to_string()).is_some_and(|v| bool_value(&v))
        }

        #[unsafe(method_id(URLForKey:))]
        fn url_for_key(&self, key: &NSString) -> Option<Retained<NSURL>> {
            self.value(&key.to_string()).and_then(|v| url_value(&v))
        }

        #[unsafe(method(setInteger:forKey:))]
        fn set_integer(&self, value: NSInteger, key: &NSString) {
            self.set(&key.to_string(), Some(Value::Integer((value as i64).into())));
        }

        #[unsafe(method(setFloat:forKey:))]
        fn set_float(&self, value: f32, key: &NSString) {
            self.set(&key.to_string(), Some(Value::Real(f64::from(value))));
        }

        #[unsafe(method(setDouble:forKey:))]
        fn set_double(&self, value: f64, key: &NSString) {
            self.set(&key.to_string(), Some(Value::Real(value)));
        }

        #[unsafe(method(setBool:forKey:))]
        fn set_bool(&self, value: bool, key: &NSString) {
            self.set(&key.to_string(), Some(Value::Boolean(value)));
        }

        #[unsafe(method(setURL:forKey:))]
        fn set_url(&self, url: Option<&NSURL>, key: &NSString) {
            self.set(&key.to_string(), url.map(url_to_value));
        }

        #[unsafe(method(registerDefaults:))]
        fn register_defaults(&self, defaults: &AnyObject) {
            if let Some(Value::Dictionary(values)) = crate::plist::from_object(defaults) {
                let mut store = crate::thread::lock_write(store());
                let registration = store.volatile.entry(REGISTRATION.to_string()).or_default();
                for (key, value) in values {
                    registration.insert(key, value);
                }
            }
        }

        #[unsafe(method(addSuiteNamed:))]
        fn add_suite_named(&self, suite: &NSString) {
            let suite = suite.to_string();
            let mut suites = crate::thread::lock(&self.ivars().suites);
            if !suites.contains(&suite) && suite != self.ivars().domain {
                suites.push(suite);
            }
        }

        #[unsafe(method(removeSuiteNamed:))]
        fn remove_suite_named(&self, suite: &NSString) {
            let suite = suite.to_string();
            crate::thread::lock(&self.ivars().suites).retain(|s| *s != suite);
        }

        #[unsafe(method_id(dictionaryRepresentation))]
        fn dictionary_representation(&self) -> Retained<AnyObject> {
            dictionary(self.merged())
        }

        #[unsafe(method_id(volatileDomainForName:))]
        fn volatile_domain_for_name(&self, name: &NSString) -> Retained<AnyObject> {
            let values = crate::thread::lock_read(store()).volatile.get(&name.to_string()).cloned().unwrap_or_default();
            dictionary(values)
        }

        #[unsafe(method(setVolatileDomain:forName:))]
        fn set_volatile_domain(&self, domain: &AnyObject, name: &NSString) {
            if let Some(Value::Dictionary(values)) = crate::plist::from_object(domain) {
                crate::thread::lock_write(store()).volatile.insert(name.to_string(), values);
            }
        }

        #[unsafe(method(removeVolatileDomainForName:))]
        fn remove_volatile_domain(&self, name: &NSString) {
            crate::thread::lock_write(store()).volatile.remove(&name.to_string());
        }

        #[cfg(feature = "collections")]
        #[unsafe(method_id(volatileDomainNames))]
        fn volatile_domain_names(&self) -> Retained<AnyObject> {
            let mut names: Vec<String> = crate::thread::lock_read(store()).volatile.keys().cloned().collect();
            names.sort_by_key(|n| (n != REGISTRATION, n != ARGUMENT, n.clone()));
            let names: Vec<Retained<NSString>> = names.iter().map(|n| NSString::from_str(n)).collect();
            objc2_foundation::NSArray::from_retained_slice(&names).into()
        }

        #[unsafe(method_id(persistentDomainForName:))]
        fn persistent_domain_for_name(&self, name: &NSString) -> Option<Retained<AnyObject>> {
            persistent_domain(&name.to_string()).map(dictionary)
        }

        #[unsafe(method(setPersistentDomain:forName:))]
        fn set_persistent_domain(&self, domain: &AnyObject, name: &NSString) {
            if let Some(Value::Dictionary(values)) = crate::plist::from_object(domain) {
                self.replace_domain(&name.to_string(), values, false);
            }
        }

        #[unsafe(method(removePersistentDomainForName:))]
        fn remove_persistent_domain(&self, name: &NSString) {
            self.replace_domain(&name.to_string(), plist::Dictionary::new(), true);
        }

        #[unsafe(method(synchronize))]
        fn synchronize(&self) -> bool {
            flush();
            true
        }

        #[unsafe(method(objectIsForcedForKey:))]
        fn object_is_forced(&self, _key: &NSString) -> bool {
            false
        }

        #[unsafe(method(objectIsForcedForKey:inDomain:))]
        fn object_is_forced_in_domain(&self, _key: &NSString, _domain: &NSString) -> bool {
            false
        }
    }

    unsafe impl NSObjectProtocol for NSUserDefaultsImpl {}
);

impl NSUserDefaultsImpl {
    /// The domains this instance searches, persistent ones marked.
    fn search_list(&self) -> Vec<(String, bool)> {
        let mut list = vec![(ARGUMENT.to_string(), false), (self.ivars().domain.clone(), true)];
        list.extend(crate::thread::lock(&self.ivars().suites).iter().map(|s| (s.clone(), true)));
        list.push((GLOBAL.to_string(), true));
        list.push((REGISTRATION.to_string(), false));
        list
    }

    fn value(&self, key: &str) -> Option<Value> {
        let list = self.search_list();
        for (name, persistent) in &list {
            if *persistent {
                ensure_loaded(name);
            }
        }
        let store = crate::thread::lock_read(store());
        list.iter().find_map(|(name, persistent)| {
            if *persistent {
                store.persistent.get(name)?.values.get(key).cloned()
            } else {
                store.volatile.get(name)?.get(key).cloned()
            }
        })
    }

    /// Everything the search list holds, earlier domains winning.
    fn merged(&self) -> plist::Dictionary {
        let list = self.search_list();
        for (name, persistent) in &list {
            if *persistent {
                ensure_loaded(name);
            }
        }
        let store = crate::thread::lock_read(store());
        let mut out = plist::Dictionary::new();
        for (name, persistent) in list.iter().rev() {
            let values =
                if *persistent { store.persistent.get(name).map(|d| &*d.values) } else { store.volatile.get(name) };
            for (key, value) in values.into_iter().flatten() {
                out.insert(key.clone(), value.clone());
            }
        }
        out
    }

    fn set(&self, key: &str, value: Option<Value>) {
        let domain = &self.ivars().domain;
        ensure_loaded(domain);
        {
            let mut store = crate::thread::lock_write(store());
            let Some(d) = store.persistent.get_mut(domain) else { return };
            match value {
                Some(value) => {
                    Arc::make_mut(&mut d.values).insert(key.to_string(), value);
                }
                None => {
                    Arc::make_mut(&mut d.values).remove(key);
                }
            }
            d.dirty = true;
            d.removed = false;
        }
        self.changed();
    }

    fn replace_domain(&self, name: &str, values: plist::Dictionary, removed: bool) {
        ensure_loaded(name);
        if let Some(d) = crate::thread::lock_write(store()).persistent.get_mut(name) {
            d.values = Arc::new(values);
            d.dirty = true;
            d.removed = removed;
        }
        self.changed();
    }

    fn changed(&self) {
        schedule_write();
        crate::notification_center::post(constant(&DID_CHANGE), Some(self.as_ref()), None);
    }
}

/// A persistent domain's values, if it has any.
fn persistent_domain(name: &str) -> Option<plist::Dictionary> {
    ensure_loaded(name);
    let store = crate::thread::lock_read(store());
    let domain = store.persistent.get(name).filter(|d| !d.removed && !d.values.is_empty())?;
    Some((*domain.values).clone())
}

fn app_domain() -> String {
    // SAFETY: +mainBundle and -bundleIdentifier return a bundle and an
    // optional string.
    let identifier: Option<Retained<NSString>> = unsafe {
        let bundle: Retained<AnyObject> = msg_send![objc2_foundation::NSBundle::class(), mainBundle];
        msg_send![&*bundle, bundleIdentifier]
    };
    identifier.map_or_else(crate::path::process_name, |i| i.to_string())
}

fn init_suite(this: Allocated<NSUserDefaultsImpl>, suite: Option<&NSString>) -> Option<Retained<NSUserDefaultsImpl>> {
    let domain = suite.map_or_else(app_domain, |s| s.to_string());
    if domain == GLOBAL {
        // A suite named after the global domain makes no sense.
        drop(this);
        return None;
    }
    Some(init_domain(this, domain))
}

/// An instance whose own domain is `domain`, any domain at all (for
/// `CFPreferences`, whose "any application" is `NSGlobalDomain`).
pub(crate) fn with_domain(domain: &str) -> Retained<objc2_foundation::NSUserDefaults> {
    // SAFETY: +alloc through the binding loads the class.
    let this: Allocated<NSUserDefaultsImpl> = unsafe { msg_send![objc2_foundation::NSUserDefaults::class(), alloc] };
    let defaults = init_domain(this, domain.to_string());
    // SAFETY: NSUserDefaultsImpl is the class NSUserDefaults names.
    unsafe { Retained::cast_unchecked(defaults) }
}

/// The application's own domain name.
pub(crate) fn application_domain() -> String {
    app_domain()
}

fn init_domain(this: Allocated<NSUserDefaultsImpl>, domain: String) -> Retained<NSUserDefaultsImpl> {
    let this = this.set_ivars(DefaultsIvars { domain, suites: Mutex::new(Vec::new()) });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn standard() -> Retained<NSUserDefaultsImpl> {
    static STANDARD: OnceLock<usize> = OnceLock::new();
    let ptr = *STANDARD.get_or_init(|| {
        // SAFETY: +alloc through the binding loads the class.
        let this: Allocated<NSUserDefaultsImpl> =
            unsafe { msg_send![objc2_foundation::NSUserDefaults::class(), alloc] };
        Retained::into_raw(init_domain(this, app_domain())) as usize
    });
    // SAFETY: the shared instance is never released.
    unsafe { Retained::retain(ptr as *mut NSUserDefaultsImpl) }.expect("the standard defaults")
}

fn dictionary(values: plist::Dictionary) -> Retained<AnyObject> {
    crate::plist::to_object(&Value::Dictionary(values))
        .unwrap_or_else(|| NSDictionary::<AnyObject, AnyObject>::new().into())
}

/// A string that is a whole decimal integer, sign allowed.
fn exact_integer(text: &str) -> Option<i64> {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A decimal number, with surrounding white space allowed.
fn decimal(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) {
        return None;
    }
    text.parse().ok()
}

pub(crate) fn integer_value(value: &Value) -> NSInteger {
    match value {
        Value::Integer(n) => n.as_signed().unwrap_or(i64::MAX) as NSInteger,
        Value::Real(r) => *r as NSInteger,
        Value::Boolean(b) => NSInteger::from(*b),
        Value::String(s) => exact_integer(s).unwrap_or(0) as NSInteger,
        _ => 0,
    }
}

pub(crate) fn double_value(value: &Value) -> f64 {
    match value {
        Value::Integer(n) => n.as_signed().map_or_else(|| n.as_unsigned().unwrap_or(0) as f64, |v| v as f64),
        Value::Real(r) => *r,
        Value::Boolean(b) => f64::from(u8::from(*b)),
        Value::String(s) => decimal(s).unwrap_or(0.0),
        _ => 0.0,
    }
}

pub(crate) fn bool_value(value: &Value) -> bool {
    match value {
        Value::Boolean(b) => *b,
        Value::Integer(n) => n.as_signed() != Some(0),
        Value::Real(r) => *r != 0.0,
        Value::String(s) => {
            s.eq_ignore_ascii_case("yes") || s.eq_ignore_ascii_case("true") || exact_integer(s) == Some(1)
        }
        _ => false,
    }
}

pub(crate) fn string_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Integer(n) => {
            Some(n.as_signed().map_or_else(|| n.as_unsigned().unwrap_or(0).to_string(), |v| v.to_string()))
        }
        Value::Real(r) => Some(format_real(*r)),
        Value::Boolean(b) => Some(if *b { "1" } else { "0" }.to_string()),
        _ => None,
    }
}

/// A real as `NSNumber` describes it: `%.15g`, near enough.
fn format_real(value: f64) -> String {
    let text = format!("{value}");
    if text.len() > 17 { format!("{value:.15}").trim_end_matches('0').trim_end_matches('.').to_string() } else { text }
}

/// A URL from a stored value: a path (tilde expanded, relative to the
/// current directory) or an archived URL.
fn url_value(value: &Value) -> Option<Retained<NSURL>> {
    match value {
        Value::String(path) => {
            let expanded = match path.strip_prefix('~') {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                    format!("{}{rest}", crate::path::without_slash(&crate::xdg::home()))
                }
                _ => path.clone(),
            };
            let absolute = if expanded.starts_with('/') {
                expanded
            } else {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
                format!("{}/{expanded}", crate::path::without_slash(&cwd).trim_end_matches('/'))
            };
            crate::url::file_url(std::path::Path::new(&crate::url::standardize(&absolute)))
        }
        Value::Data(bytes) => {
            let (archive, _) = crate::plist::parse(bytes)?;
            let string = archived_url(&archive)?;
            crate::url::make(string, None).map(crate::url::as_url)
        }
        _ => None,
    }
}

/// How `setURL:forKey:` stores a URL.
fn url_to_value(url: &NSURL) -> Value {
    if let Some(path) = crate::url::file_path(url) {
        let path = crate::path::without_slash(&path);
        let home = crate::path::without_slash(&crate::xdg::home());
        return Value::String(match path.strip_prefix(&home) {
            Some(rest) if home != "/" && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
            _ => path,
        });
    }
    let string = crate::url::url_impl(url).absolute().0.to_string();
    Value::Data(crate::plist::write_binary(&keyed_archive(&string)).unwrap_or_default())
}

/// An `NSKeyedArchiver` archive of a URL, the layout macOS writes.
fn keyed_archive(string: &str) -> Value {
    let uid = |n| Value::Uid(plist::Uid::new(n));
    let mut url = plist::Dictionary::new();
    url.insert("NS.base".into(), uid(0));
    url.insert("NS.relative".into(), uid(2));
    url.insert("$class".into(), uid(3));
    let mut class = plist::Dictionary::new();
    class.insert("$classname".into(), Value::String("NSURL".into()));
    class
        .insert("$classes".into(), Value::Array(vec![Value::String("NSURL".into()), Value::String("NSObject".into())]));
    let mut top = plist::Dictionary::new();
    top.insert("root".into(), uid(1));
    let mut archive = plist::Dictionary::new();
    archive.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
    archive.insert("$version".into(), Value::Integer(100_000.into()));
    archive.insert("$top".into(), Value::Dictionary(top));
    archive.insert(
        "$objects".into(),
        Value::Array(vec![
            Value::String("$null".into()),
            Value::Dictionary(url),
            Value::String(string.into()),
            Value::Dictionary(class),
        ]),
    );
    Value::Dictionary(archive)
}

/// The URL string in a keyed archive of an `NSURL` without a base.
fn archived_url(archive: &Value) -> Option<String> {
    let archive = archive.as_dictionary()?;
    let objects = archive.get("$objects")?.as_array()?;
    let root = archive.get("$top")?.as_dictionary()?.get("root")?.as_uid()?.get() as usize;
    let url = objects.get(root)?.as_dictionary()?;
    let relative = url.get("NS.relative")?.as_uid()?.get() as usize;
    objects.get(relative)?.as_string().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coercions() {
        let s = |t: &str| Value::String(t.into());
        for (text, int, double, boolean) in [
            ("42", 42, 42.0, false),
            ("abc", 0, 0.0, false),
            ("3.7", 0, 3.7, false),
            ("YES", 0, 0.0, true),
            ("yes", 0, 0.0, true),
            ("true", 0, 0.0, true),
            ("1", 1, 1.0, true),
            ("NO", 0, 0.0, false),
            ("2", 2, 2.0, false),
            (" 5 ", 0, 5.0, false),
            ("Y", 0, 0.0, false),
            ("-3", -3, -3.0, false),
            ("1e3", 0, 1000.0, false),
            ("0x10", 0, 0.0, false),
        ] {
            assert_eq!(integer_value(&s(text)), int, "{text:?}");
            assert_eq!(double_value(&s(text)), double, "{text:?}");
            assert_eq!(bool_value(&s(text)), boolean, "{text:?}");
        }
        assert_eq!(integer_value(&Value::Real(-2.9)), -2);
        assert!(bool_value(&Value::Integer(2.into())));
        assert_eq!(string_value(&Value::Real(2.9)).as_deref(), Some("2.9"));
        assert_eq!(string_value(&Value::Boolean(true)).as_deref(), Some("1"));
        assert_eq!(
            archived_url(&keyed_archive("http://example.com/a%20b")).as_deref(),
            Some("http://example.com/a%20b")
        );
    }

    #[test]
    fn arguments() {
        let args = ["-NSShowAll", "YES", "--flag", "x", "plain", "-list", "(a, b)", "-last"].map(String::from);
        let domain = argument_domain(args.into_iter());
        assert_eq!(domain.get("NSShowAll"), Some(&Value::String("YES".into())));
        assert_eq!(domain.get("list"), Some(&Value::Array(vec![Value::String("a".into()), Value::String("b".into())])));
        assert_eq!(domain.get("-flag"), None);
        assert_eq!(domain.get("last"), None, "a key needs a value");
    }
}
