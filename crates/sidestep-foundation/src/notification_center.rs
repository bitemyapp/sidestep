//! `NSNotificationCenter`, and [`post`] for Sidestep's own frameworks.
//!
//! A center keeps its registrations in a slab under one mutex, in one of
//! four lists each: by name hash and then object address (registrations
//! with both), by name hash (a name and any object), by object address (an
//! object and any name) and a list of wildcards; a fifth map finds an
//! observer's registrations for removal. A post looks at the four lists
//! its name and object can match, so it costs what it delivers, not what
//! others registered for other objects. It collects the matches, lets go
//! of the lock and then calls them in registration order, which is the
//! order macOS uses across all kinds of registration (see
//! `conformance/tests/notifications.rs`).
//! No lock is held during a callout, so observers may post, add and remove
//! on any thread; a registration removed during a post is skipped from
//! then on, and one added during a post waits for the next.
//!
//! Selector observers are held weakly: the center doesn't keep them alive,
//! and one that goes away without unregistering is simply no longer
//! called. Names match by string equality, objects by identity, and
//! objects aren't retained. Block observers are tokens the center keeps
//! (with their blocks) until they are removed.
//!
//! Posting costs one atomic load when a center has no observers, and no
//! allocation when nothing matches: the `NSNotification` is only made for
//! a post someone receives. AppKit posts view frame changes through
//! [`post`] on every resize, so this matters.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSCopying, NSNotification, NSNotificationCenter, NSString};

use crate::string::fast_parts;
use crate::thread::lock;

/// Hashes that are already hashes (string hashes) or addresses, mixed so
/// that aligned addresses spread.
#[derive(Default)]
struct Mixed(u64);

impl Hasher for Mixed {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(8) ^ u64::from(b)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
    }

    fn write_usize(&mut self, n: usize) {
        self.0 = (n as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(29);
    }
}

type Map = HashMap<usize, Vec<u32>, BuildHasherDefault<Mixed>>;

/// The registrations for one name hash.
#[derive(Default)]
struct Named {
    /// For any object.
    any: Vec<u32>,
    /// For one object, by its address.
    by_object: Map,
}

sidestep_runtime::static_class!(pub(crate) NSNOTIFICATIONCENTER, NSNOTIFICATIONCENTER_META = "NSNotificationCenter", || {
    let _ = NSNotificationCenterImpl::class();
    crate::perform::install();
});

/// What a registration calls.
enum Target {
    Selector { observer: Weak<AnyObject>, selector: Sel },
    Block(Retained<Token>),
}

/// One registration, shared between the index and posts in flight.
struct Registration {
    seq: u64,
    /// Its slot in the index while it is registered.
    slot: u32,
    removed: AtomicBool,
    target: Target,
}

// SAFETY: the weak reference and the token are only loaded and messaged,
// which the runtime makes thread-safe; blocks run on the posting thread, as
// on macOS, and whatever they capture is their owner's business.
unsafe impl Send for Registration {}
unsafe impl Sync for Registration {}

struct Entry {
    reg: Arc<Registration>,
    /// The name's hash and the name, or none for any name.
    name: Option<(usize, Retained<NSString>)>,
    /// The object's address, or 0 for any object.
    object: usize,
    /// The observer's (or token's) address, for removal.
    observer: usize,
}

#[derive(Default)]
struct Index {
    slots: Vec<Option<Entry>>,
    free: Vec<u32>,
    named: HashMap<usize, Named, BuildHasherDefault<Mixed>>,
    objects: Map,
    wildcard: Vec<u32>,
    observers: Map,
    seq: u64,
}

// SAFETY: see `Registration`; the names are immutable strings.
unsafe impl Send for Index {}

/// The hash and, for Sidestep's strings, the text of a name.
fn name_hash(name: &NSString) -> usize {
    match fast_parts(name) {
        Some((_, hash)) => hash,
        None => name.hash(),
    }
}

fn same_name(a: &NSString, b: &NSString) -> bool {
    if std::ptr::eq(a, b) {
        return true;
    }
    match (fast_parts(a), fast_parts(b)) {
        (Some((a, _)), Some((b, _))) => a == b,
        _ => a.isEqualToString(b),
    }
}

fn address(object: Option<&AnyObject>) -> usize {
    object.map_or(0, |o| o as *const AnyObject as usize)
}

impl Index {
    /// A free slot for the next registration.
    fn reserve(&mut self) -> u32 {
        match self.free.pop() {
            Some(slot) => slot,
            None => {
                self.slots.push(None);
                u32::try_from(self.slots.len() - 1).expect("sidestep: too many notification observers")
            }
        }
    }

    /// Fill the slot [`reserve`](Self::reserve) gave, and list it.
    fn insert(&mut self, slot: u32, entry: Entry) {
        match &entry.name {
            Some((hash, _)) => {
                let named = self.named.entry(*hash).or_default();
                match entry.object {
                    0 => named.any.push(slot),
                    object => named.by_object.entry(object).or_default().push(slot),
                }
            }
            None if entry.object != 0 => self.objects.entry(entry.object).or_default().push(slot),
            None => self.wildcard.push(slot),
        }
        self.observers.entry(entry.observer).or_default().push(slot);
        self.slots[slot as usize] = Some(entry);
    }

    /// Take out a slot's entry, from every list it is in.
    fn take(&mut self, slot: u32) -> Option<Entry> {
        let entry = self.slots[slot as usize].take()?;
        fn unlist(map: &mut Map, key: usize, slot: u32) {
            if let Some(list) = map.get_mut(&key) {
                list.retain(|&s| s != slot);
                if list.is_empty() {
                    map.remove(&key);
                }
            }
        }
        match &entry.name {
            Some((hash, _)) => {
                if let Some(named) = self.named.get_mut(hash) {
                    match entry.object {
                        0 => named.any.retain(|&s| s != slot),
                        object => unlist(&mut named.by_object, object, slot),
                    }
                    if named.any.is_empty() && named.by_object.is_empty() {
                        self.named.remove(hash);
                    }
                }
            }
            None if entry.object != 0 => unlist(&mut self.objects, entry.object, slot),
            None => self.wildcard.retain(|&s| s != slot),
        }
        unlist(&mut self.observers, entry.observer, slot);
        entry.reg.removed.store(true, Ordering::Release);
        self.free.push(slot);
        Some(entry)
    }

    /// The registrations a post of `name` by `object` reaches, in
    /// registration order.
    fn matches(&self, name: &NSString, hash: usize, object: usize, out: &mut Vec<Arc<Registration>>) {
        let mut sources = 0;
        if let Some(named) = self.named.get(&hash) {
            // Equal hashes may still be different names.
            let mut named_list = |list: &[u32]| {
                let before = out.len();
                for &slot in list {
                    let entry = self.slots[slot as usize].as_ref().expect("listed slots are full");
                    let (_, entry_name) = entry.name.as_ref().expect("named entries have names");
                    if same_name(entry_name, name) {
                        out.push(entry.reg.clone());
                    }
                }
                sources += usize::from(out.len() > before);
            };
            named_list(&named.any);
            if object != 0
                && let Some(list) = named.by_object.get(&object)
            {
                named_list(list);
            }
        }
        if object != 0
            && let Some(list) = self.objects.get(&object)
        {
            out.extend(list.iter().map(|&slot| self.slots[slot as usize].as_ref().unwrap().reg.clone()));
            sources += 1;
        }
        if !self.wildcard.is_empty() {
            out.extend(self.wildcard.iter().map(|&slot| self.slots[slot as usize].as_ref().unwrap().reg.clone()));
            sources += 1;
        }
        if sources > 1 {
            out.sort_unstable_by_key(|reg| reg.seq);
        }
    }
}

pub(crate) struct CenterIvars {
    index: Mutex<Index>,
    /// Registrations in the index, so a center nobody observes costs one
    /// load per post.
    count: AtomicUsize,
}

impl CenterIvars {
    fn new() -> Self {
        CenterIvars { index: Mutex::new(Index::default()), count: AtomicUsize::new(0) }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSNotificationCenter"]
    #[ivars = CenterIvars]
    pub(crate) struct NSNotificationCenterImpl;

    impl NSNotificationCenterImpl {
        #[unsafe(method_id(defaultCenter))]
        fn default_center_method() -> Retained<Self> {
            default_center_impl()
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(CenterIvars::new());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(addObserver:selector:name:object:))]
        fn add_observer(&self, observer: &AnyObject, selector: Sel, name: Option<&NSString>, object: Option<&AnyObject>) {
            let target = Target::Selector { observer: Weak::new(observer), selector };
            self.register(target, address(Some(observer)), name, object);
        }

        #[unsafe(method_id(addObserverForName:object:queue:usingBlock:))]
        fn add_observer_for_name(
            &self,
            name: Option<&NSString>,
            object: Option<&AnyObject>,
            queue: Option<&AnyObject>,
            block: &DynBlock<dyn Fn(NonNull<NSNotification>)>,
        ) -> Retained<AnyObject> {
            let token = Token::new(block.copy(), queue.map(|q| q.retain()));
            let key = address(Some(token.as_ref()));
            self.register(Target::Block(token.clone()), key, name, object);
            token.into_super().into()
        }

        #[unsafe(method(postNotification:))]
        fn post_notification(&self, notification: &NSNotification) {
            let name = notification.name();
            let object = notification.object();
            self.post(&name, object.as_deref(), Posted::Made(notification));
        }

        #[unsafe(method(postNotificationName:object:))]
        fn post_name(&self, name: &NSString, object: Option<&AnyObject>) {
            self.post(name, object, Posted::Lazy(None));
        }

        #[unsafe(method(postNotificationName:object:userInfo:))]
        fn post_name_user_info(&self, name: &NSString, object: Option<&AnyObject>, user_info: Option<&AnyObject>) {
            self.post(name, object, Posted::Lazy(user_info));
        }

        #[unsafe(method(removeObserver:))]
        fn remove_observer(&self, observer: &AnyObject) {
            self.remove(observer, None, None);
        }

        #[unsafe(method(removeObserver:name:object:))]
        fn remove_observer_name(&self, observer: &AnyObject, name: Option<&NSString>, object: Option<&AnyObject>) {
            self.remove(observer, name, object);
        }
    }

    unsafe impl NSObjectProtocol for NSNotificationCenterImpl {}
);

/// The notification a post delivers: given, or made on first delivery.
enum Posted<'a> {
    Made(&'a NSNotification),
    Lazy(Option<&'a AnyObject>),
}

impl NSNotificationCenterImpl {
    fn register(&self, target: Target, observer: usize, name: Option<&NSString>, object: Option<&AnyObject>) {
        let name = name.map(|n| (name_hash(n), n.copy()));
        let mut index = lock(&self.ivars().index);
        index.seq += 1;
        let slot = index.reserve();
        let reg = Arc::new(Registration { seq: index.seq, slot, removed: AtomicBool::new(false), target });
        index.insert(slot, Entry { reg, name, object: address(object), observer });
        self.ivars().count.fetch_add(1, Ordering::Release);
    }

    /// Remove `observer`'s registrations for `name` and `object` (`None`
    /// matching any). A block observer's token goes whatever the filters,
    /// as on macOS.
    fn remove(&self, observer: &AnyObject, name: Option<&NSString>, object: Option<&AnyObject>) {
        let key = address(Some(observer));
        let token = observer.downcast_ref::<Token>().is_some();
        let hash = name.map(name_hash);
        let object = address(object);
        let removed: Vec<Entry> = {
            let mut index = lock(&self.ivars().index);
            let Some(slots) = index.observers.get(&key).cloned() else { return };
            let chosen: Vec<u32> = slots
                .into_iter()
                .filter(|&slot| {
                    let entry = index.slots[slot as usize].as_ref().unwrap();
                    let name_ok = match (name, hash, &entry.name) {
                        (None, _, _) => true,
                        (Some(n), Some(h), Some((eh, en))) => h == *eh && same_name(n, en),
                        _ => false,
                    };
                    token || (name_ok && (object == 0 || entry.object == object))
                })
                .collect();
            chosen.into_iter().filter_map(|slot| index.take(slot)).collect()
        };
        self.ivars().count.fetch_sub(removed.len(), Ordering::Release);
        // Dropped here, unlocked: a token's block may hold the last
        // reference to anything.
        drop(removed);
    }

    fn post(&self, name: &NSString, object: Option<&AnyObject>, posted: Posted<'_>) {
        if self.ivars().count.load(Ordering::Acquire) == 0 {
            return;
        }
        thread_local!(static SCRATCH: std::cell::Cell<Vec<Arc<Registration>>> = const { std::cell::Cell::new(Vec::new()) });
        // A post from a thread-local destructor may find the scratch gone.
        let mut matches = SCRATCH.try_with(|s| s.take()).unwrap_or_default();
        let hash = name_hash(name);
        lock(&self.ivars().index).matches(name, hash, address(object), &mut matches);
        if !matches.is_empty() {
            let made;
            let note: &NSNotification = match posted {
                Posted::Made(note) => note,
                Posted::Lazy(user_info) => {
                    made = crate::notification::notification_with(name, object, user_info);
                    &made
                }
            };
            // Observers that went away without unregistering; rare, so
            // only then is this list allocated.
            let mut dead = Vec::new();
            for reg in &matches {
                if !reg.removed.load(Ordering::Acquire) && !deliver(reg, note) {
                    dead.push(reg.clone());
                }
            }
            if !dead.is_empty() {
                self.forget_dead(&dead);
            }
            matches.clear();
        }
        let _ = SCRATCH.try_with(|s| {
            let previous = s.replace(matches);
            if previous.capacity() > 0 {
                // A nested post returned its own list first; keep that one.
                s.set(previous);
            }
        });
    }

    /// Drop the registrations of observers that have gone away, by their
    /// slots (those still theirs: another post may have got there first,
    /// and the slot been reused since).
    fn forget_dead(&self, dead: &[Arc<Registration>]) {
        let removed: Vec<Entry> = {
            let mut index = lock(&self.ivars().index);
            dead.iter()
                .filter_map(|reg| {
                    let listed = index.slots[reg.slot as usize].as_ref().is_some_and(|e| Arc::ptr_eq(&e.reg, reg));
                    if listed { index.take(reg.slot) } else { None }
                })
                .collect()
        };
        self.ivars().count.fetch_sub(removed.len(), Ordering::Release);
    }
}

/// Call one registration; false if its observer has gone away.
fn deliver(reg: &Registration, note: &NSNotification) -> bool {
    match &reg.target {
        Target::Selector { observer, selector } => {
            let Some(observer) = observer.load() else { return false };
            // SAFETY: an observer's selector takes the notification.
            unsafe { crate::perform::send_object(&observer, *selector, Some(note.as_ref())) };
        }
        Target::Block(token) => token.call(note),
    }
    true
}

pub(crate) struct TokenIvars {
    block: RcBlock<dyn Fn(NonNull<NSNotification>)>,
    queue: Option<Retained<AnyObject>>,
}

// The token `addObserverForName:object:queue:usingBlock:` returns. The
// center keeps it, and with it the block, until it is removed.
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepNotificationObserver"]
    #[ivars = TokenIvars]
    pub(crate) struct Token;

    unsafe impl NSObjectProtocol for Token {}
);

impl Token {
    fn new(block: RcBlock<dyn Fn(NonNull<NSNotification>)>, queue: Option<Retained<AnyObject>>) -> Retained<Token> {
        let this = Token::alloc().set_ivars(TokenIvars { block, queue });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn call(&self, note: &NSNotification) {
        let ivars = self.ivars();
        match &ivars.queue {
            None => ivars.block.call((NonNull::from(note),)),
            Some(queue) => crate::operation::run_and_wait(queue, &ivars.block, note),
        }
    }
}

struct DefaultCenter(*const NSNotificationCenterImpl);

// SAFETY: the default center is never released, and its state is behind a
// mutex and atomics.
unsafe impl Send for DefaultCenter {}
unsafe impl Sync for DefaultCenter {}

static DEFAULT: OnceLock<DefaultCenter> = OnceLock::new();

fn default_center_impl() -> Retained<NSNotificationCenterImpl> {
    let center = DEFAULT.get_or_init(|| {
        // Load the class the NSNotificationCenter shell names first.
        // SAFETY: +class takes nothing and returns the receiver.
        let _: *const objc2::runtime::AnyClass = unsafe { msg_send![NSNotificationCenter::class(), class] };
        let this = NSNotificationCenterImpl::alloc().set_ivars(CenterIvars::new());
        // SAFETY: NSObject's designated initializer.
        let center: Retained<NSNotificationCenterImpl> = unsafe { msg_send![super(this), init] };
        DefaultCenter(Retained::into_raw(center))
    });
    // SAFETY: the default center is immortal.
    unsafe { Retained::retain(center.0.cast_mut()) }.expect("the default center exists")
}

/// Post on the default center, as `-postNotificationName:object:userInfo:`
/// does, but without making a notification or sending a message when nobody
/// observes: for frameworks that post often.
pub fn post(name: &NSString, object: Option<&AnyObject>, user_info: Option<&AnyObject>) {
    let Some(center) = DEFAULT.get() else { return };
    // SAFETY: the default center is immortal.
    let center = unsafe { &*center.0 };
    center.post(name, object, Posted::Lazy(user_info));
}

/// The default center.
pub fn default_center() -> Retained<NSNotificationCenter> {
    // SAFETY: the implementation class is the class NSNotificationCenter
    // names.
    unsafe { Retained::cast_unchecked(default_center_impl()) }
}

/// Whether anyone observes `name` on the default center (with any object).
pub fn has_observers(name: &NSString) -> bool {
    let Some(center) = DEFAULT.get() else { return false };
    // SAFETY: the default center is immortal.
    let center = unsafe { &*center.0 };
    if center.ivars().count.load(Ordering::Acquire) == 0 {
        return false;
    }
    let index = lock(&center.ivars().index);
    !index.wildcard.is_empty() || !index.objects.is_empty() || index.named.contains_key(&name_hash(name))
}
