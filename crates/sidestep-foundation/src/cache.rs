//! `NSCache`: a thread-safe store of objects by key that lets go of them
//! when it holds more than its limits, least recently used first.
//!
//! Foundation makes caches safe to use from any thread at once, so a
//! cache's state sits behind a mutex. Entries live in a slab (a vector with
//! a free list, so an entry keeps its place for as long as it lives),
//! threaded on a list from least to most recently used, with an index of
//! their places by the key's hash (`hash_index.rs`). A key is hashed before
//! the lock is taken; keys that are Sidestep's strings or numbers are then
//! compared without messages, others with `-isEqual:` under the lock (so a
//! key's `-isEqual:` must not use the cache). Keys are retained, not
//! copied, as in Foundation.
//!
//! Adding an object, or lowering a limit, evicts the least recently used
//! entries until the cache is within its count and total cost limits (0 is
//! none); an object whose cost alone passes the cost limit is evicted too.
//! Reading an object makes it the most recently used. The delegate hears
//! `cache:willEvictObject:` for every object that leaves, by eviction,
//! replacement, removal, or the cache's own deallocation. Nothing runs
//! other code under the lock: objects leaving are collected there and
//! told to the delegate, then released, after it is let go, so a delegate
//! may use the cache. The delegate is held weakly.
//!
//! `evictsObjectsWithDiscardedContent` is kept, but `NSDiscardableContent`
//! objects aren't treated specially yet.

use std::cell::Cell;
use std::ptr;
use std::sync::{Mutex, MutexGuard};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_foundation::{NSString, NSUInteger};

use crate::hash_index::Index;
use crate::pointer_table::WeakCell;
use crate::table::Probe;
use crate::util;

sidestep_runtime::static_class!(pub(crate) NSCACHE, NSCACHE_META = "NSCache", || {
    let _ = NSCacheImpl::class();
});

/// No entry.
const NONE: u32 = u32::MAX;

struct Node {
    hash: NSUInteger,
    key: Retained<AnyObject>,
    value: Retained<AnyObject>,
    cost: usize,
    /// The next entry used less, and more, recently.
    older: u32,
    newer: u32,
}

/// An object leaving the cache, to tell the delegate of and release once
/// the lock is let go.
type Leaving = Vec<(Retained<AnyObject>, Retained<AnyObject>)>;

struct State {
    /// The entries; `None` in free places.
    nodes: Vec<Option<Node>>,
    free: Vec<u32>,
    /// Places by the key's hash.
    index: Index,
    len: usize,
    /// The least and the most recently used.
    oldest: u32,
    newest: u32,
    total_cost: usize,
    count_limit: usize,
    cost_limit: usize,
    evicts_discarded: bool,
    name: Option<Retained<NSString>>,
    delegate: Option<WeakCell>,
}

impl Default for State {
    fn default() -> Self {
        State {
            nodes: Vec::new(),
            free: Vec::new(),
            index: Index::default(),
            len: 0,
            oldest: NONE,
            newest: NONE,
            total_cost: 0,
            count_limit: 0,
            cost_limit: 0,
            evicts_discarded: true,
            name: None,
            delegate: None,
        }
    }
}

impl State {
    fn node(&self, at: u32) -> &Node {
        self.nodes[at as usize].as_ref().expect("a live entry")
    }

    fn node_mut(&mut self, at: u32) -> &mut Node {
        self.nodes[at as usize].as_mut().expect("a live entry")
    }

    /// The place of the entry for the key `probe` is made from. Compares
    /// keys with `-isEqual:` where they aren't Sidestep's strings or
    /// numbers.
    fn find(&self, probe: &Probe) -> Option<u32> {
        if self.index.is_empty() {
            return None;
        }
        self.index
            .find(probe.hash, |at| {
                let node = self.nodes[at].as_ref().expect("indexed entries are live");
                probe.matches_key(node.hash, &node.key)
            })
            .ok()
            .map(|at| at as u32)
    }

    fn unlink(&mut self, at: u32) {
        let (older, newer) = {
            let node = self.node(at);
            (node.older, node.newer)
        };
        if older == NONE {
            self.oldest = newer;
        } else {
            self.node_mut(older).newer = newer;
        }
        if newer == NONE {
            self.newest = older;
        } else {
            self.node_mut(newer).older = older;
        }
    }

    fn push_newest(&mut self, at: u32) {
        let newest = self.newest;
        {
            let node = self.node_mut(at);
            node.older = newest;
            node.newer = NONE;
        }
        match newest {
            NONE => self.oldest = at,
            n => self.node_mut(n).newer = at,
        }
        self.newest = at;
    }

    /// Make the entry at `at` the most recently used.
    fn touch(&mut self, at: u32) {
        if self.newest != at {
            self.unlink(at);
            self.push_newest(at);
        }
    }

    fn insert(&mut self, hash: NSUInteger, key: Retained<AnyObject>, value: Retained<AnyObject>, cost: usize) {
        let node = Node { hash, key, value, cost, older: NONE, newer: NONE };
        let at = match self.free.pop() {
            Some(at) => {
                self.nodes[at as usize] = Some(node);
                at
            }
            None => {
                self.nodes.push(Some(node));
                (self.nodes.len() - 1) as u32
            }
        };
        self.len += 1;
        self.total_cost += cost;
        if self.index.full_for(self.len) {
            let nodes = &self.nodes;
            let live = nodes.iter().enumerate().filter_map(|(i, n)| n.as_ref().map(|n| (i, n.hash)));
            self.index.rebuild_with(Index::size_for(self.len.max(8)), live);
        } else {
            self.index.add(hash, at as usize);
        }
        self.push_newest(at);
    }

    /// Take out the entry at `at`, with its key and object.
    fn take(&mut self, at: u32) -> (Retained<AnyObject>, Retained<AnyObject>) {
        self.unlink(at);
        let node = self.nodes[at as usize].take().expect("a live entry");
        let nodes = &self.nodes;
        self.index.remove(node.hash, at as usize, |p| nodes[p].as_ref().expect("indexed entries are live").hash);
        self.free.push(at);
        self.len -= 1;
        self.total_cost -= node.cost;
        (node.key, node.value)
    }

    /// Evict the least recently used until within the limits.
    fn evict(&mut self, leaving: &mut Leaving) {
        while self.oldest != NONE
            && ((self.count_limit > 0 && self.len > self.count_limit)
                || (self.cost_limit > 0 && self.total_cost > self.cost_limit))
        {
            let oldest = self.oldest;
            leaving.push(self.take(oldest));
        }
    }

    /// Take out every entry, least recently used first.
    fn clear(&mut self) -> Leaving {
        let mut leaving = Vec::with_capacity(self.len);
        while self.oldest != NONE {
            let oldest = self.oldest;
            leaving.push(self.take(oldest));
        }
        self.nodes.clear();
        self.free.clear();
        self.index = Index::default();
        leaving
    }
}

pub(crate) struct CacheIvars {
    state: Mutex<State>,
    /// This cache, for telling the delegate as it deallocates.
    this: Cell<*const AnyObject>,
}

impl Drop for CacheIvars {
    fn drop(&mut self) {
        // The cache is deallocating: what it holds leaves.
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        let leaving = state.clear();
        let delegate = state.delegate.take();
        if !leaving.is_empty() {
            tell(self.this.get(), delegate.as_ref().and_then(WeakCell::load), leaving);
        }
    }
}

/// Tell `delegate` that each object leaving is about to leave the cache
/// `this`, then release them all.
fn tell(this: *const AnyObject, delegate: Option<Retained<AnyObject>>, leaving: Leaving) {
    if let Some(delegate) = &delegate
        && delegate.class().responds_to(sel!(cache:willEvictObject:))
    {
        for (_, object) in &leaving {
            // SAFETY: the NSCacheDelegate method takes the cache and the
            // object and returns nothing; the cache is alive (or, from
            // `Drop`, not yet freed), as the object is.
            let _: () = unsafe { msg_send![&**delegate, cache: this, willEvictObject: &**object] };
        }
    }
    drop(leaving);
}

fn init(this: Allocated<NSCacheImpl>) -> Retained<NSCacheImpl> {
    let at = Allocated::as_ptr(&this).cast::<AnyObject>();
    let this = this.set_ivars(CacheIvars { state: Mutex::default(), this: Cell::new(at) });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

impl NSCacheImpl {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic under the lock (a key's -isEqual:) leaves the state whole.
        self.ivars().state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn obj(&self) -> *const AnyObject {
        ptr::from_ref(self).cast()
    }

    /// Tell the delegate of what is leaving and release it, the lock let go.
    fn finish(&self, state: MutexGuard<'_, State>, leaving: Leaving) {
        if leaving.is_empty() {
            return;
        }
        let delegate = state.delegate.as_ref().and_then(WeakCell::load);
        drop(state);
        tell(self.obj(), delegate, leaving);
    }

    fn set(&self, object: &AnyObject, key: &AnyObject, cost: usize) {
        // Retained and hashed before the lock: either may run code.
        let (object, key) = (object.retain(), key.retain());
        let probe = Probe::new(&key);
        let hash = probe.hash;
        let mut state = self.lock();
        let mut leaving = Vec::new();
        match state.find(&probe) {
            Some(at) => {
                let node = state.node_mut(at);
                let old = std::mem::replace(&mut node.value, object);
                let old_cost = std::mem::replace(&mut node.cost, cost);
                let key = node.key.clone();
                state.total_cost = state.total_cost - old_cost + cost;
                state.touch(at);
                leaving.push((key, old));
            }
            None => state.insert(hash, key, object, cost),
        }
        state.evict(&mut leaving);
        self.finish(state, leaving);
    }

    fn get(&self, key: &AnyObject) -> Option<Retained<AnyObject>> {
        // Hashed before the lock: -hash may run code.
        let probe = Probe::new(key);
        let mut state = self.lock();
        let at = state.find(&probe)?;
        state.touch(at);
        Some(state.node(at).value.clone())
    }

    /// `-setObject:forKey:cost:`: a nil key is ignored, as in Foundation; a
    /// nil object fails.
    fn checked_set(&self, object: Option<&AnyObject>, key: Option<&AnyObject>, cost: usize) {
        let Some(key) = key else { return };
        let Some(object) = object else {
            panic!("-[NSCache setObject:forKey:cost:]: attempt to insert nil value (key: {})", util::description(key));
        };
        self.set(object, key, cost);
    }

    fn set_limits(&self, count: Option<usize>, cost: Option<usize>) {
        let mut state = self.lock();
        if let Some(count) = count {
            state.count_limit = count;
        }
        if let Some(cost) = cost {
            state.cost_limit = cost;
        }
        let mut leaving = Vec::new();
        state.evict(&mut leaving);
        self.finish(state, leaving);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSCache"]
    #[ivars = CacheIvars]
    pub(crate) struct NSCacheImpl;

    impl NSCacheImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init(this)
        }

        /// Retained and autoreleased: another thread may evict it at once.
        #[unsafe(method_id(objectForKey:))]
        fn object_for_key(&self, key: Option<&AnyObject>) -> Option<Retained<AnyObject>> {
            key.and_then(|key| self.get(key))
        }

        #[unsafe(method(setObject:forKey:))]
        fn set_object_for_key(&self, object: Option<&AnyObject>, key: Option<&AnyObject>) {
            self.checked_set(object, key, 0);
        }

        #[unsafe(method(setObject:forKey:cost:))]
        fn set_object_for_key_cost(&self, object: Option<&AnyObject>, key: Option<&AnyObject>, cost: NSUInteger) {
            self.checked_set(object, key, cost);
        }

        #[unsafe(method(removeObjectForKey:))]
        fn remove_object_for_key(&self, key: Option<&AnyObject>) {
            let Some(key) = key else { return };
            let probe = Probe::new(key);
            let mut state = self.lock();
            let Some(at) = state.find(&probe) else { return };
            let leaving = vec![state.take(at)];
            self.finish(state, leaving);
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all_objects(&self) {
            let mut state = self.lock();
            let leaving = state.clear();
            self.finish(state, leaving);
        }

        #[unsafe(method(countLimit))]
        fn count_limit(&self) -> NSUInteger {
            self.lock().count_limit
        }

        #[unsafe(method(setCountLimit:))]
        fn set_count_limit(&self, limit: NSUInteger) {
            self.set_limits(Some(limit), None);
        }

        #[unsafe(method(totalCostLimit))]
        fn total_cost_limit(&self) -> NSUInteger {
            self.lock().cost_limit
        }

        #[unsafe(method(setTotalCostLimit:))]
        fn set_total_cost_limit(&self, limit: NSUInteger) {
            self.set_limits(None, Some(limit));
        }

        #[unsafe(method(evictsObjectsWithDiscardedContent))]
        fn evicts_objects_with_discarded_content(&self) -> bool {
            self.lock().evicts_discarded
        }

        #[unsafe(method(setEvictsObjectsWithDiscardedContent:))]
        fn set_evicts_objects_with_discarded_content(&self, evicts: bool) {
            self.lock().evicts_discarded = evicts;
        }

        /// The empty string until given one.
        #[unsafe(method_id(name))]
        fn name(&self) -> Retained<NSString> {
            let name = self.lock().name.clone();
            name.unwrap_or_else(|| NSString::from_str(""))
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: &NSString) {
            // Copied before the lock: copying may run code.
            // SAFETY: a copy of an NSString is an NSString.
            let name = unsafe { Retained::cast_unchecked::<NSString>(util::copy_key(name)) };
            let old = self.lock().name.replace(name);
            drop(old);
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.lock().delegate.as_ref().and_then(WeakCell::load)
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            let cell = delegate.map(WeakCell::new);
            let old = std::mem::replace(&mut self.lock().delegate, cell);
            drop(old);
        }
    }

    unsafe impl NSObjectProtocol for NSCacheImpl {}
);
