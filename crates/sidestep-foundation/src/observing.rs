//! Scalar key-value observation with explicit will/did change delivery.
//! Framework setters opt in through `will_change` / `did_change`; arbitrary
//! application setters are not dynamically replaced. Dotted dependency paths
//! and collection mutation notifications are not implemented yet.
use crate::thread::lock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSDictionary, NSNull, NSNumber, NSString};
use std::{collections::HashMap, ffi::c_void, sync::Mutex};

crate::constant_string!(NSKeyValueChangeKindKey = "kind");
crate::constant_string!(NSKeyValueChangeNewKey = "new");
crate::constant_string!(NSKeyValueChangeOldKey = "old");
crate::constant_string!(NSKeyValueChangeIndexesKey = "indexes");
crate::constant_string!(NSKeyValueChangeNotificationIsPriorKey = "notificationIsPrior");

#[derive(Clone)]
struct Entry {
    observer: Weak<AnyObject>,
    key: String,
    options: usize,
    context: usize,
}
#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    pending: HashMap<String, Vec<Option<Retained<AnyObject>>>>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepKeyValueObservers"]
    #[ivars = Mutex<State>]
    struct Store;
);
static KEY: u8 = 0;
static LOOKUP: Mutex<()> = Mutex::new(());
fn store(object: &AnyObject, create: bool) -> Option<Retained<Store>> {
    let _guard = lock(&LOOKUP);
    let key = (&KEY as *const u8).cast();
    let object = std::ptr::from_ref(object).cast_mut();
    // SAFETY: the association key is private and its value is always Store.
    let found = unsafe { objc2::ffi::objc_getAssociatedObject(object, key) };
    if !found.is_null() {
        return unsafe { Retained::retain(found.cast::<Store>().cast_mut()) };
    }
    if !create {
        return None;
    }
    let value = Store::alloc().set_ivars(Mutex::new(State::default()));
    let value: Retained<Store> = unsafe { msg_send![super(value), init] };
    unsafe {
        objc2::ffi::objc_setAssociatedObject(
            object,
            key,
            Retained::as_ptr(&value).cast_mut().cast(),
            objc2::ffi::OBJC_ASSOCIATION_RETAIN,
        );
    }
    Some(value)
}
fn value(object: &AnyObject, key: &str) -> Option<Retained<AnyObject>> {
    crate::sort_descriptor::property(object, key)
}
fn notify(
    object: &AnyObject,
    entry: &Entry,
    old: Option<&AnyObject>,
    new: Option<&AnyObject>,
    prior: bool,
    initial: bool,
) {
    let Some(observer) = entry.observer.load() else { return };
    let mut keys = vec![NSString::from_str("kind")];
    let mut values: Vec<Retained<AnyObject>> = vec![NSNumber::new_usize(1).into()];
    if entry.options & 2 != 0 && !initial {
        keys.push(NSString::from_str("old"));
        values.push(old.map(Message::retain).unwrap_or_else(|| NSNull::null().into()));
    }
    if entry.options & 1 != 0 && !prior {
        keys.push(NSString::from_str("new"));
        values.push(new.map(Message::retain).unwrap_or_else(|| NSNull::null().into()));
    }
    if prior {
        keys.push(NSString::from_str("notificationIsPrior"));
        values.push(NSNumber::new_bool(true).into());
    }
    let change = NSDictionary::from_retained_objects(&keys.iter().map(|k| &**k).collect::<Vec<_>>(), &values);
    // SAFETY: registration requires this public observer selector and a live context.
    unsafe {
        let _: () = msg_send![&*observer, observeValueForKeyPath: &*NSString::from_str(&entry.key), ofObject: object, change: &*change, context: entry.context as *mut c_void];
    }
}
/// Pair this with `did_change` around a framework property mutation.
pub fn will_change(object: &AnyObject, key: &str) {
    let Some(store) = store(object, false) else { return };
    let entries: Vec<_> = lock(store.ivars()).entries.iter().filter(|e| e.key == key).cloned().collect();
    if entries.is_empty() {
        return;
    }
    let old = value(object, key);
    lock(store.ivars()).pending.entry(key.into()).or_default().push(old.clone());
    for entry in entries.iter().filter(|e| e.options & 8 != 0) {
        notify(object, entry, old.as_deref(), None, true, false);
    }
}
/// Deliver after dropping any lock or borrow protecting the changed property.
pub fn did_change(object: &AnyObject, key: &str) {
    let Some(store) = store(object, false) else { return };
    let (old, entries) = {
        let mut state = lock(store.ivars());
        let Some(old) = state.pending.get_mut(key).and_then(Vec::pop) else { return };
        let entries: Vec<_> = state.entries.iter().filter(|e| e.key == key).cloned().collect();
        (old, entries)
    };
    let new = value(object, key);
    for entry in entries {
        notify(object, &entry, old.as_deref(), new.as_deref(), false, false);
    }
}
fn receiver(this: &Observing) -> &AnyObject {
    unsafe { &*std::ptr::from_ref(this).cast() }
}
fn remove(object: &AnyObject, observer: &AnyObject, key: &NSString, context: Option<usize>) {
    if let Some(store) = store(object, false) {
        let key = key.to_string();
        lock(store.ivars()).entries.retain(|e| {
            let Some(live) = e.observer.load() else { return false };
            !(std::ptr::eq(&*live, observer) && e.key == key && context.is_none_or(|c| c == e.context))
        });
    }
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepObservingMethods"]
    struct Observing;
    impl Observing {
        #[unsafe(method(addObserver:forKeyPath:options:context:))]
        fn add(&self, observer: &AnyObject, key: &NSString, options: usize, context: *mut c_void) {
            let object = receiver(self);
            let entry = Entry { observer: Weak::new(observer), key: key.to_string(), options, context: context as usize };
            let store = store(object, true).unwrap();
            lock(store.ivars()).entries.push(entry.clone());
            if options & 4 != 0 { notify(object, &entry, None, value(object, &entry.key).as_deref(), false, true); }
        }
        #[unsafe(method(removeObserver:forKeyPath:context:))]
        fn remove_context(&self, observer: &AnyObject, key: &NSString, context: *mut c_void) { remove(receiver(self), observer, key, Some(context as usize)); }
        #[unsafe(method(removeObserver:forKeyPath:))]
        fn remove(&self, observer: &AnyObject, key: &NSString) { remove(receiver(self), observer, key, None); }
        #[unsafe(method(willChangeValueForKey:))]
        fn will(&self, key: &NSString) { will_change(receiver(self), &key.to_string()); }
        #[unsafe(method(didChangeValueForKey:))]
        fn did(&self, key: &NSString) { did_change(receiver(self), &key.to_string()); }
    }
);
sidestep_runtime::category!("NSObject"(SidestepObserving), |category| {
    // SAFETY: these methods use only the receiver's associated store.
    unsafe { category.add_methods_of(Observing::class()) };
});
