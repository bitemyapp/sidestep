//! `NSHashTable`, `NSMapTable` and `NSPointerFunctions`: collections whose
//! members, keys and values are held as their pointer functions say, most
//! usefully weakly.
//!
//! `NSPointerFunctionsOptions` choose, for each side, a memory (strong
//! references, weak references, or bare pointers the collection doesn't
//! own), a personality (objects compared by `-hash` and `-isEqual:`, objects
//! compared by address, bare pointers, or integers) and whether objects
//! are copied in. An [`Item`] holds one pointer as its [`Functions`] say.
//! Other memories (`malloc`'d, Mach virtual memory) and personalities (C
//! strings, structs) aren't supported and fail when a collection is made.
//! Pointers that aren't objects are never retained, whatever the memory;
//! objects in opaque memory aren't either, but are still handed out as
//! objects (by `-allObjects`, enumerators and descriptions).
//!
//! A weak item is a weak reference through the runtime's `objc_initWeak`
//! and `objc_loadWeakRetained`, in a box of its own so its address stays
//! put while the table's entries move. When the object deallocates, the
//! runtime zeroes the reference, and the entry is dead: lookups, counts,
//! enumeration and descriptions pass over it at once, and the table sweeps
//! dead entries out (releasing, say, a strong value whose weak key went)
//! as it changes, at most once per as many changes as it has entries, so
//! a change still costs constant time on average. Telling a weak item dead
//! reads its location atomically without loading it: Sidestep's runtime
//! reads and writes weak locations only atomically (`arc.rs`), and a
//! location it has zeroed never holds that object again.
//!
//! The tables themselves follow the other collections: entries in a vector
//! with their hashes and an index of positions by hash (`hash_index.rs`),
//! in a [`Guarded`] cell so any number of threads may read at once and a
//! callback that changes the table it was called from fails loudly. A
//! lookup compares hashes first, so a weak key is loaded (retained) only
//! when its hash matches; objects it loads are released after the table
//! is let go, since releasing may run code. Enumerators and fast
//! enumeration of weak tables hand out a snapshot of the live objects,
//! which keeps them alive for the loop, and still fail if the table
//! changes meanwhile.

use std::cell::{Cell, UnsafeCell};
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, Ordering};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSDictionary, NSEnumerator, NSFastEnumerationState, NSHashTable, NSMapTable, NSPointerFunctions,
    NSPointerFunctionsOptions, NSSet, NSString, NSUInteger, NSZone,
};

use crate::enumerator::{self, Mutations, Source};
use crate::guarded::Guarded;
use crate::hash_index::{self, Index};
use crate::table::Probe;
use crate::util::{self, inherits};
use crate::{array, dictionary, set};

sidestep_runtime::static_class!(pub(crate) NSHASHTABLE, NSHASHTABLE_META = "NSHashTable", || {
    let _ = NSHashTableImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSMAPTABLE, NSMAPTABLE_META = "NSMapTable", || {
    let _ = NSMapTableImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSPOINTERFUNCTIONS, NSPOINTERFUNCTIONS_META = "NSPointerFunctions", || {
    let _ = NSPointerFunctionsImpl::class();
});

/// How a side of a collection holds its pointers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Memory {
    Strong,
    Weak,
    /// Bare pointers, neither retained nor released.
    Opaque,
}

/// How a side of a collection hashes and compares its pointers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Personality {
    /// Objects, by `-hash` and `-isEqual:`.
    Object,
    /// Objects, by address.
    ObjectPointer,
    /// Bare pointers, by address.
    Opaque,
    /// Integers, by value.
    Integer,
}

/// The write barrier a side reports using (`-usesStrongWriteBarrier`,
/// `-usesWeakReadAndWriteBarriers`). Barriers served garbage collection:
/// setting one changes what is reported, as in Foundation, never how the
/// side holds its pointers, which its memory decides.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Barrier {
    None,
    Strong,
    Weak,
}

/// A side's `NSPointerFunctionsOptions`, understood.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Functions {
    pub(crate) memory: Memory,
    pub(crate) personality: Personality,
    pub(crate) copy_in: bool,
    pub(crate) barrier: Barrier,
}

const WEAK_MEMORY: NSUInteger = 5;
const ZEROING_WEAK_MEMORY: NSUInteger = 1;
const OPAQUE_MEMORY: NSUInteger = 2;
const OPAQUE_PERSONALITY: NSUInteger = 1 << 8;
const OBJECT_POINTER_PERSONALITY: NSUInteger = 2 << 8;
const INTEGER_PERSONALITY: NSUInteger = 5 << 8;
const COPY_IN: NSUInteger = 1 << 16;

impl Functions {
    pub(crate) const STRONG: Functions =
        Functions { memory: Memory::Strong, personality: Personality::Object, copy_in: false, barrier: Barrier::None };
    pub(crate) const WEAK: Functions =
        Functions { memory: Memory::Weak, personality: Personality::Object, copy_in: false, barrier: Barrier::Weak };

    /// The functions `options` ask for, failing as `receiver`'s `method`
    /// for options Sidestep doesn't support.
    pub(crate) fn from_options(options: NSPointerFunctionsOptions, receiver: &str, method: &str) -> Functions {
        let bits = options.0;
        let unsupported = || -> ! {
            panic!("*** -[{receiver} {method}]: unsupported NSPointerFunctionsOptions {bits:#x}");
        };
        let memory = match bits & 0xff {
            0 => Memory::Strong,
            ZEROING_WEAK_MEMORY | WEAK_MEMORY => Memory::Weak,
            OPAQUE_MEMORY => Memory::Opaque,
            _ => unsupported(),
        };
        let personality = match bits & (0xff << 8) {
            0 => Personality::Object,
            OPAQUE_PERSONALITY => Personality::Opaque,
            OBJECT_POINTER_PERSONALITY => Personality::ObjectPointer,
            INTEGER_PERSONALITY => Personality::Integer,
            _ => unsupported(),
        };
        if bits & !(0xffff | COPY_IN) != 0 {
            unsupported();
        }
        // Only objects are retained, or referenced weakly.
        let memory = match personality {
            Personality::Opaque | Personality::Integer => Memory::Opaque,
            _ => memory,
        };
        let barrier = if memory == Memory::Weak { Barrier::Weak } else { Barrier::None };
        Functions { memory, personality, copy_in: bits & COPY_IN != 0, barrier }
    }

    /// Whether this side holds objects (and so may hand them out as such).
    pub(crate) fn holds_objects(self) -> bool {
        matches!(self.personality, Personality::Object | Personality::ObjectPointer)
    }
}

/// A weak reference in a place of its own, which the runtime zeroes when
/// the object deallocates.
pub(crate) struct WeakCell(Box<UnsafeCell<*mut AnyObject>>);

impl WeakCell {
    pub(crate) fn new(obj: &AnyObject) -> Self {
        let cell = Box::new(UnsafeCell::new(ptr::null_mut()));
        // SAFETY: a fresh location for objc_initWeak, which stays put (it is
        // boxed) until objc_destroyWeak in `drop`; `obj` is alive.
        unsafe { objc2::ffi::objc_initWeak(cell.get(), ptr::from_ref(obj).cast_mut()) };
        WeakCell(cell)
    }

    /// The object's address, or null once it is gone. Never to be
    /// dereferenced: the object may be deallocating.
    #[inline]
    pub(crate) fn peek(&self) -> *mut AnyObject {
        // SAFETY: the location is valid and aligned; Sidestep's runtime
        // reads and writes weak locations only atomically, so this load
        // races with nothing.
        unsafe { AtomicPtr::from_ptr(self.0.get()) }.load(Ordering::Relaxed)
    }

    /// The object, retained, unless it is gone or going.
    pub(crate) fn load(&self) -> Option<Retained<AnyObject>> {
        // SAFETY: an initialized weak location; objc_loadWeakRetained
        // returns +1 or null.
        unsafe { Retained::from_raw(objc2::ffi::objc_loadWeakRetained(self.0.get())) }
    }
}

impl Drop for WeakCell {
    fn drop(&mut self) {
        // SAFETY: an initialized weak location, destroyed once.
        unsafe { objc2::ffi::objc_destroyWeak(self.0.get()) };
    }
}

/// One pointer, held as its functions say.
pub(crate) enum Item {
    Strong(Retained<AnyObject>),
    Weak(WeakCell),
    /// An object the collection doesn't own (opaque memory with an object
    /// personality): its owner keeps it alive while it is in the
    /// collection, which hands it out as an object.
    Unowned(NonNull<AnyObject>),
    /// A pointer that isn't an object, or null.
    Raw(*mut c_void),
}

/// The pointer a map table's value side holds in a hash table's entries.
const NOTHING: Item = Item::Raw(ptr::null_mut());

impl Item {
    /// Hold `pointer` as `functions` say, copying an object first if they
    /// copy in.
    ///
    /// # Safety
    /// If `functions` hold objects, `pointer` must be a live object.
    pub(crate) unsafe fn new(functions: Functions, pointer: *mut c_void) -> Item {
        if functions.memory == Memory::Opaque {
            return match NonNull::new(pointer.cast::<AnyObject>()) {
                Some(obj) if functions.holds_objects() => Item::Unowned(obj),
                _ => Item::Raw(pointer),
            };
        }
        // SAFETY: guaranteed by the caller.
        let obj = unsafe { &*pointer.cast::<AnyObject>() };
        let obj = if functions.copy_in { util::copy_key(obj) } else { obj.retain() };
        match functions.memory {
            Memory::Weak => Item::Weak(WeakCell::new(&obj)),
            _ => Item::Strong(obj),
        }
    }

    /// The pointer, without loading a weak one: null once a weak item's
    /// object is gone. Not to be dereferenced if weak.
    #[inline]
    pub(crate) fn pointer(&self) -> *mut c_void {
        match self {
            Item::Strong(obj) => Retained::as_ptr(obj).cast_mut().cast(),
            Item::Weak(cell) => cell.peek().cast(),
            Item::Unowned(obj) => obj.as_ptr().cast(),
            Item::Raw(p) => *p,
        }
    }

    /// Whether this is a weak item whose object is gone.
    #[inline]
    pub(crate) fn is_dead(&self) -> bool {
        matches!(self, Item::Weak(cell) if cell.peek().is_null())
    }

    /// The object, retained; `None` for a weak item whose object is gone
    /// (or going) and for pointers that aren't objects.
    pub(crate) fn load(&self) -> Option<Retained<AnyObject>> {
        match self {
            Item::Strong(obj) => Some(obj.clone()),
            Item::Weak(cell) => cell.load(),
            // SAFETY: its owner keeps the object alive while it is in the
            // collection.
            Item::Unowned(obj) => Some(unsafe { obj.as_ref() }.retain()),
            Item::Raw(_) => None,
        }
    }

    /// The same pointer held the same way, for a copy of the collection;
    /// `None` for a weak item whose object is gone. What it loads goes on
    /// `loaded`, for the caller to release once it lets the collection go.
    pub(crate) fn duplicate(&self, loaded: &mut Vec<Retained<AnyObject>>) -> Option<Item> {
        Some(match self {
            Item::Strong(obj) => Item::Strong(obj.clone()),
            Item::Weak(cell) => {
                let obj = cell.load()?;
                let copy = Item::Weak(WeakCell::new(&obj));
                loaded.push(obj);
                copy
            }
            Item::Unowned(obj) => Item::Unowned(*obj),
            Item::Raw(p) => Item::Raw(*p),
        })
    }

    /// What a caller of `-member:`, `-objectForKey:` or `-pointerAtIndex:`
    /// gets: a strong item's object unretained (the collection keeps it
    /// alive), a weak item's loaded and autoreleased (nothing else keeps
    /// it alive), an unowned object or a bare pointer as it is.
    pub(crate) fn handed_out(&self) -> *mut c_void {
        match self {
            Item::Weak(cell) => cell.load().map_or(ptr::null_mut(), |o| Retained::autorelease_ptr(o).cast()),
            item => item.pointer(),
        }
    }
}

/// What a lookup looks for: the pointer, its hash, and for objects compared
/// by `-isEqual:`, how to compare it quickly.
pub(crate) struct Wanted<'a> {
    pointer: *mut c_void,
    pub(crate) hash: NSUInteger,
    probe: Option<Probe<'a>>,
}

impl<'a> Wanted<'a> {
    /// # Safety
    /// If `functions` compare objects, `pointer` must be an object alive
    /// for `'a`.
    pub(crate) unsafe fn new(functions: Functions, pointer: *mut c_void) -> Wanted<'a> {
        match functions.personality {
            Personality::Object => {
                // SAFETY: guaranteed by the caller.
                let probe = Probe::new(unsafe { &*pointer.cast::<AnyObject>() });
                Wanted { pointer, hash: probe.hash, probe: Some(probe) }
            }
            _ => Wanted { pointer, hash: pointer as NSUInteger, probe: None },
        }
    }
}

pub(crate) struct Entry {
    pub(crate) hash: NSUInteger,
    pub(crate) key: Item,
    /// `NOTHING` in a hash table.
    pub(crate) value: Item,
}

impl Entry {
    fn is_dead(&self) -> bool {
        self.key.is_dead() || self.value.is_dead()
    }
}

/// Up to this many entries, a table has no index.
const SCAN: usize = 8;

/// An object loaded from an item, if it could be.
type Loaded = Option<Retained<AnyObject>>;

/// A hash or map table's entries.
#[derive(Default)]
pub(crate) struct Table {
    entries: Vec<Entry>,
    index: Index,
    /// Whether any side holds weak references, which may die.
    weak: bool,
    /// Entries added since the last sweep for dead entries, and how many
    /// entries that sweep left.
    since_sweep: usize,
    after_sweep: usize,
}

impl Table {
    fn new(weak: bool) -> Table {
        Table { weak, ..Table::default() }
    }

    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The live entries.
    pub(crate) fn live(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |e| !self.weak || !e.is_dead())
    }

    pub(crate) fn live_count(&self) -> usize {
        if self.weak { self.live().count() } else { self.entries.len() }
    }

    /// Whether `entry`'s key is what `wanted` looks for. May send
    /// `-isEqual:`, and loads weak keys, pushing them onto `loaded` for the
    /// caller to release once it lets the table go.
    fn matches(entry: &Entry, wanted: &Wanted, loaded: &mut Vec<Retained<AnyObject>>) -> bool {
        if entry.hash != wanted.hash {
            return false;
        }
        let pointer = entry.key.pointer();
        if pointer.is_null() {
            // A weak key gone (or a null pointer, which no lookup wants).
            return pointer == wanted.pointer && !matches!(entry.key, Item::Weak(_));
        }
        if pointer == wanted.pointer {
            return true;
        }
        let Some(probe) = &wanted.probe else { return false };
        match &entry.key {
            Item::Strong(obj) => probe.matches_key(entry.hash, obj),
            Item::Weak(cell) => match cell.load() {
                Some(obj) => {
                    let verdict = probe.matches_key(entry.hash, &obj);
                    loaded.push(obj);
                    verdict
                }
                None => false,
            },
            // SAFETY: an object the table doesn't own, which its owner
            // keeps alive while it is in the table.
            Item::Unowned(obj) => probe.matches_key(entry.hash, unsafe { obj.as_ref() }),
            // Pointers that aren't objects compare by address alone.
            Item::Raw(_) => false,
        }
    }

    /// The position of the entry whose key is what `wanted` looks for.
    pub(crate) fn find(&self, wanted: &Wanted, loaded: &mut Vec<Retained<AnyObject>>) -> Option<usize> {
        if self.index.is_empty() {
            return self.entries.iter().position(|e| Self::matches(e, wanted, loaded));
        }
        self.index.find(wanted.hash, |at| Self::matches(&self.entries[at], wanted, loaded)).ok()
    }

    fn reindex(&mut self) {
        if self.entries.len() <= SCAN {
            self.index.clear();
            return;
        }
        let entries = &self.entries;
        self.index.rebuild(hash_index::size_for(entries.len()), entries.len(), |at| entries[at].hash);
    }

    /// Add an entry for a key known to be absent. Returns entries swept out
    /// meanwhile, for the caller to release once it lets the table go.
    #[must_use]
    pub(crate) fn insert(&mut self, hash: NSUInteger, key: Item, value: Item) -> Vec<Entry> {
        let swept = self.maybe_sweep();
        let at = self.entries.len();
        self.entries.push(Entry { hash, key, value });
        if self.index.is_empty() {
            if at + 1 > SCAN {
                self.reindex();
            }
        } else if self.index.full_for(at + 1) {
            self.reindex();
        } else {
            self.index.add(hash, at);
        }
        swept
    }

    /// Take out the entry at `at`; the last entry moves into its place.
    pub(crate) fn remove(&mut self, at: usize) -> Entry {
        if !self.index.is_empty() {
            let entries = &self.entries;
            self.index.remove(entries[at].hash, at, |p| entries[p].hash);
            let last = entries.len() - 1;
            if at != last {
                self.index.moved(entries[last].hash, last, at);
            }
        }
        self.entries.swap_remove(at)
    }

    pub(crate) fn value_mut(&mut self, at: usize) -> &mut Item {
        &mut self.entries[at].value
    }

    /// Take out every entry.
    pub(crate) fn clear(&mut self) -> Vec<Entry> {
        self.index.clear();
        (self.since_sweep, self.after_sweep) = (0, 0);
        std::mem::take(&mut self.entries)
    }

    /// Sweep dead entries out once more entries have been added since the
    /// last sweep than it left. A sweep costs time in proportion to the
    /// entries, so this is constant time per addition on average, and a
    /// table whose members keep dying holds at most about twice as many
    /// entries as were alive at the last sweep.
    fn maybe_sweep(&mut self) -> Vec<Entry> {
        if !self.weak {
            return Vec::new();
        }
        self.since_sweep += 1;
        if self.since_sweep <= self.after_sweep {
            return Vec::new();
        }
        let (live, dead): (Vec<Entry>, Vec<Entry>) =
            std::mem::take(&mut self.entries).into_iter().partition(|e| !e.is_dead());
        self.entries = live;
        (self.since_sweep, self.after_sweep) = (0, self.entries.len());
        self.reindex();
        dead
    }

    /// How many entries the table holds, dead ones included.
    #[cfg(test)]
    fn held(&self) -> usize {
        self.entries.len()
    }

    /// A copy with the same pointers, held the same way, and what making it
    /// loaded (see `Item::duplicate`).
    fn duplicate(&self) -> (Table, Vec<Retained<AnyObject>>) {
        let mut copy = Table::new(self.weak);
        let mut loaded = Vec::new();
        for e in self.live() {
            if let (Some(key), Some(value)) = (e.key.duplicate(&mut loaded), e.value.duplicate(&mut loaded)) {
                let swept = copy.insert(e.hash, key, value);
                debug_assert!(swept.is_empty());
            }
        }
        (copy, loaded)
    }
}

/// A pointer as a description shows it, taken out of the table first
/// (releasing a loaded object may run code).
enum Shown {
    Object(Retained<AnyObject>),
    Integer(isize),
    Pointer(*mut c_void),
    Gone,
}

impl Shown {
    fn of(item: &Item, functions: Functions) -> Shown {
        if functions.holds_objects() {
            item.load().map_or(Shown::Gone, Shown::Object)
        } else if functions.personality == Personality::Integer {
            Shown::Integer(item.pointer() as isize)
        } else {
            Shown::Pointer(item.pointer())
        }
    }

    fn write(&self, out: &mut String) {
        use std::fmt::Write;
        match self {
            Shown::Object(obj) => out.push_str(&util::description(obj)),
            Shown::Integer(v) => {
                let _ = write!(out, "{v}");
            }
            Shown::Pointer(p) => {
                let _ = write!(out, "{:p}", *p);
            }
            Shown::Gone => {}
        }
    }
}

/// The description of a hash table (`values` is `None`) or map table.
fn describe_table(
    name: &str,
    table: &Guarded<Table>,
    keys: Functions,
    values: Option<Functions>,
) -> Retained<NSString> {
    let rows: Vec<(usize, Shown, Option<Shown>)> = table
        .read()
        .entries()
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.is_dead())
        .map(|(i, e)| (i, Shown::of(&e.key, keys), values.map(|f| Shown::of(&e.value, f))))
        .collect();
    let mut out = format!("{name} {{\n");
    for (i, key, value) in &rows {
        if matches!(key, Shown::Gone) || matches!(value, Some(Shown::Gone)) {
            continue;
        }
        out.push_str(&format!("[{i}] "));
        key.write(&mut out);
        if let Some(value) = value {
            out.push_str(" -> ");
            value.write(&mut out);
        }
        out.push('\n');
    }
    out.push_str("}\n");
    NSString::from_str(&out)
}

/// The objects of a side of a table's live entries, retained (a snapshot
/// that keeps them alive). Loaded with the table read, released after.
fn objects(table: &Guarded<Table>, values: bool) -> Vec<Retained<AnyObject>> {
    let loaded: Vec<Option<Retained<AnyObject>>> =
        table.read().live().map(|e| if values { e.value.load() } else { e.key.load() }).collect();
    loaded.into_iter().flatten().collect()
}

/// Fast enumeration of a table's keys (or members): straight from its
/// entries when it holds them strongly or not at all, else from a snapshot
/// of the live ones, made on the first call and kept (autoreleased) by the
/// state.
///
/// # Safety
/// `state` must be valid and `buffer` must have room for `len` pointers.
unsafe fn fast_enumerate(
    table: &Guarded<Table>,
    keys: Functions,
    mutations: &Mutations,
    state: NonNull<NSFastEnumerationState>,
    buffer: NonNull<*mut AnyObject>,
    len: usize,
) -> NSUInteger {
    if keys.memory == Memory::Weak {
        // SAFETY: guaranteed by the caller.
        return unsafe {
            enumerator::gathered(state, buffer, len, mutations.as_ptr(), || array::make(objects(table, false)))
        };
    }
    // SAFETY: copying pointers into the buffer runs no other code.
    let table = unsafe { table.peek() };
    let entries = table.entries();
    // SAFETY: guaranteed by the caller.
    let st = unsafe { &mut *state.as_ptr() };
    // The state is the position of the next entry to look at: entries
    // whose weak value is gone are passed over.
    let (mut at, mut n) = (st.state as usize, 0);
    while n < len && at < entries.len() {
        let entry = &entries[at];
        at += 1;
        if table.weak && entry.is_dead() {
            continue;
        }
        // SAFETY: `n < len`, within the caller's buffer.
        unsafe { buffer.as_ptr().add(n).write(entry.key.pointer().cast()) };
        n += 1;
    }
    st.state = at as _;
    st.itemsPtr = buffer.as_ptr();
    st.mutationsPtr = mutations.as_ptr();
    n
}

// NSHashTable

pub(crate) struct HashTableIvars {
    functions: Functions,
    table: Guarded<Table>,
    mutations: Mutations,
}

impl HashTableIvars {
    fn new(functions: Functions) -> Self {
        HashTableIvars {
            functions,
            table: Guarded::new(Table::new(functions.memory == Memory::Weak)),
            mutations: Mutations::default(),
        }
    }
}

const HASH_TABLE: &str = "NSHashTable";

fn make_hash_table(functions: Functions) -> Retained<NSHashTable> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSHashTable::<AnyObject>::alloc();
    // SAFETY: NSHashTable's class is NSHashTableImpl.
    let this = unsafe { std::mem::transmute::<Allocated<NSHashTable>, Allocated<NSHashTableImpl>>(this) };
    let this = this.set_ivars(HashTableIvars::new(functions));
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSHashTableImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSHashTableImpl is the class registered as NSHashTable.
    unsafe { Retained::cast_unchecked(this) }
}

fn init_hash_table(this: Allocated<NSHashTableImpl>, functions: Functions) -> Retained<NSHashTableImpl> {
    let this = this.set_ivars(HashTableIvars::new(functions));
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// Sidestep's storage of a hash table, a subclass's included.
fn hash_table(obj: &AnyObject) -> Option<&NSHashTableImpl> {
    // SAFETY: an instance of NSHashTableImpl or a subclass, whose ivars sit
    // where NSHashTableImpl's methods expect them.
    inherits(obj, &NSHASHTABLE).then(|| unsafe { &*(obj as *const AnyObject).cast::<NSHashTableImpl>() })
}

/// The functions an `NSPointerFunctions` object stands for.
pub(crate) fn functions_of(pointer_functions: &NSPointerFunctions) -> Functions {
    match inherits(pointer_functions, &NSPOINTERFUNCTIONS) {
        // SAFETY: an instance of NSPointerFunctionsImpl or a subclass.
        true => unsafe { &*(ptr::from_ref(pointer_functions).cast::<NSPointerFunctionsImpl>()) }.ivars().get(),
        false => Functions::STRONG,
    }
}

impl NSHashTableImpl {
    fn obj(&self) -> *const AnyObject {
        ptr::from_ref(self).cast()
    }

    /// The position of the member `pointer` is equal to, with what the
    /// lookup loaded, to release once nothing is held.
    fn find(&self, pointer: *mut c_void) -> (Option<usize>, Vec<Retained<AnyObject>>) {
        let ivars = self.ivars();
        // SAFETY: callers pass members of the table's kind: live objects
        // where it compares objects.
        let wanted = unsafe { Wanted::new(ivars.functions, pointer) };
        let mut loaded = Vec::new();
        let found = ivars.table.read().find(&wanted, &mut loaded);
        (found, loaded)
    }

    fn contains(&self, pointer: *mut c_void) -> bool {
        self.find(pointer).0.is_some()
    }

    /// `-addObject:`: a member equal to `pointer` stays as it is.
    fn add(&self, pointer: *mut c_void) {
        let ivars = self.ivars();
        // SAFETY: as in `find`.
        let wanted = unsafe { Wanted::new(ivars.functions, pointer) };
        let mut loaded = Vec::new();
        if ivars.table.read().find(&wanted, &mut loaded).is_some() {
            return;
        }
        // Retained or copied before the table is taken: copying may run code.
        // SAFETY: as in `find`.
        let item = unsafe { Item::new(ivars.functions, pointer) };
        // SAFETY: only the table changes; what it sweeps out is released
        // after.
        let swept = unsafe { ivars.table.write(HASH_TABLE, self.obj()) }.insert(wanted.hash, item, NOTHING);
        ivars.mutations.bump();
        drop((swept, loaded));
    }

    fn remove(&self, pointer: *mut c_void) {
        let (found, loaded) = self.find(pointer);
        let Some(at) = found else { return };
        let ivars = self.ivars();
        // SAFETY: only the table changes; the entry is released after.
        let removed = unsafe { ivars.table.write(HASH_TABLE, self.obj()) }.remove(at);
        ivars.mutations.bump();
        drop((removed, loaded));
    }

    fn clear(&self) {
        let ivars = self.ivars();
        // SAFETY: only the table changes; the entries are released after.
        let removed = unsafe { ivars.table.write(HASH_TABLE, self.obj()) }.clear();
        ivars.mutations.bump();
        drop(removed);
    }

    /// The live members' pointers: objects retained, for calls that may
    /// run code.
    fn members(&self) -> Vec<Member> {
        let loaded: Vec<Option<Member>> = self
            .ivars()
            .table
            .read()
            .live()
            .map(|e| match &e.key {
                Item::Raw(p) => Some(Member::Raw(*p)),
                key => key.load().map(Member::Object),
            })
            .collect();
        loaded.into_iter().flatten().collect()
    }
}

/// A member taken out of a hash table to work with.
enum Member {
    Object(Retained<AnyObject>),
    Raw(*mut c_void),
}

impl Member {
    fn pointer(&self) -> *mut c_void {
        match self {
            Member::Object(obj) => Retained::as_ptr(obj).cast_mut().cast(),
            Member::Raw(p) => *p,
        }
    }
}

/// Whether `other` has a member equal to `pointer`.
fn hash_table_contains(other: &AnyObject, pointer: *mut c_void) -> bool {
    match hash_table(other) {
        Some(table) => table.contains(pointer),
        // SAFETY: -containsObject: takes an object and returns BOOL.
        None => unsafe { msg_send![other, containsObject: pointer.cast::<AnyObject>()] },
    }
}

fn hash_table_count(other: &AnyObject) -> usize {
    match hash_table(other) {
        Some(table) => table.ivars().table.read().live_count(),
        // SAFETY: -count takes nothing and returns NSUInteger.
        None => unsafe { msg_send![other, count] },
    }
}

fn hash_table_members(other: &AnyObject) -> Vec<Member> {
    match hash_table(other) {
        Some(table) => table.members(),
        None => {
            // SAFETY: -allObjects returns an array of the members.
            let all: Retained<NSArray> = unsafe { msg_send![other, allObjects] };
            all.to_vec().into_iter().map(Member::Object).collect()
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSHashTable"]
    #[ivars = HashTableIvars]
    pub(crate) struct NSHashTableImpl;

    impl NSHashTableImpl {
        #[unsafe(method_id(hashTableWithOptions:))]
        fn hash_table_with_options(options: NSPointerFunctionsOptions) -> Retained<NSHashTable> {
            make_hash_table(Functions::from_options(options, HASH_TABLE, "initWithOptions:capacity:"))
        }

        #[unsafe(method_id(weakObjectsHashTable))]
        fn weak_objects_hash_table() -> Retained<NSHashTable> {
            make_hash_table(Functions::WEAK)
        }

        #[unsafe(method_id(hashTableWithWeakObjects))]
        fn hash_table_with_weak_objects() -> Retained<AnyObject> {
            util::upcast(make_hash_table(Functions::WEAK))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_hash_table(this, Functions::STRONG)
        }

        /// The capacity is a hint, which Sidestep's tables don't need.
        #[unsafe(method_id(initWithOptions:capacity:))]
        fn init_with_options(this: Allocated<Self>, options: NSPointerFunctionsOptions, _capacity: NSUInteger) -> Retained<Self> {
            init_hash_table(this, Functions::from_options(options, HASH_TABLE, "initWithOptions:capacity:"))
        }

        #[unsafe(method_id(initWithPointerFunctions:capacity:))]
        fn init_with_pointer_functions(
            this: Allocated<Self>,
            functions: &NSPointerFunctions,
            _capacity: NSUInteger,
        ) -> Retained<Self> {
            init_hash_table(this, functions_of(functions))
        }

        #[unsafe(method_id(pointerFunctions))]
        fn pointer_functions(&self) -> Retained<NSPointerFunctions> {
            make_pointer_functions(self.ivars().functions)
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            let table = &self.ivars().table;
            // SAFETY: counting reads no member and runs no other code.
            unsafe { table.peek() }.live_count()
        }

        #[unsafe(method(member:))]
        fn member(&self, object: *mut AnyObject) -> *mut AnyObject {
            if object.is_null() {
                return ptr::null_mut();
            }
            let (found, loaded) = self.find(object.cast());
            // Read with the table counted: handing out a weak member
            // autoreleases it, which may run code.
            let member = found.map_or(ptr::null_mut(), |at| self.ivars().table.read().entries()[at].key.handed_out().cast());
            drop(loaded);
            member
        }

        #[unsafe(method(containsObject:))]
        fn contains_object(&self, object: *mut AnyObject) -> bool {
            !object.is_null() && self.contains(object.cast())
        }

        /// Nil is ignored, as in Foundation.
        #[unsafe(method(addObject:))]
        fn add_object(&self, object: *mut AnyObject) {
            if !object.is_null() {
                self.add(object.cast());
            }
        }

        #[unsafe(method(removeObject:))]
        fn remove_object(&self, object: *mut AnyObject) {
            if !object.is_null() {
                self.remove(object.cast());
            }
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all_objects(&self) {
            self.clear();
        }

        #[unsafe(method_id(allObjects))]
        fn all_objects(&self) -> Retained<NSArray> {
            array::make(objects(&self.ivars().table, false))
        }

        #[unsafe(method(anyObject))]
        fn any_object(&self) -> *mut AnyObject {
            let table = self.ivars().table.read();
            let first = table.live().next().map_or(ptr::null_mut(), |e| e.key.handed_out());
            first.cast()
        }

        #[unsafe(method_id(setRepresentation))]
        fn set_representation(&self) -> Retained<NSSet> {
            set::make_of(objects(&self.ivars().table, false))
        }

        #[unsafe(method_id(objectEnumerator))]
        fn object_enumerator(&self) -> Retained<NSEnumerator> {
            let ivars = self.ivars();
            let snapshot = array::make(objects(&ivars.table, false));
            enumerator::make(Source::snapshot(snapshot, util::upcast(self.retain()), ivars.mutations.as_ptr()))
        }

        #[unsafe(method(intersectsHashTable:))]
        fn intersects_hash_table(&self, other: &NSHashTable) -> bool {
            self.members().iter().any(|m| hash_table_contains(other, m.pointer()))
        }

        #[unsafe(method(isSubsetOfHashTable:))]
        fn is_subset_of_hash_table(&self, other: &NSHashTable) -> bool {
            self.members().iter().all(|m| hash_table_contains(other, m.pointer()))
        }

        #[unsafe(method(isEqualToHashTable:))]
        fn is_equal_to_hash_table(&self, other: &NSHashTable) -> bool {
            let members = self.members();
            members.len() == hash_table_count(other) && members.iter().all(|m| hash_table_contains(other, m.pointer()))
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| {
                hash_table(other).is_some() && {
                    let members = self.members();
                    members.len() == hash_table_count(other)
                        && members.iter().all(|m| hash_table_contains(other, m.pointer()))
                }
            })
        }

        /// The count, as in Foundation.
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            self.ivars().table.read().live_count()
        }

        #[unsafe(method(intersectHashTable:))]
        fn intersect_hash_table(&self, other: &NSHashTable) {
            for member in self.members() {
                if !hash_table_contains(other, member.pointer()) {
                    self.remove(member.pointer());
                }
            }
        }

        #[unsafe(method(unionHashTable:))]
        fn union_hash_table(&self, other: &NSHashTable) {
            // Gathered first: `other` may be this table.
            for member in hash_table_members(other) {
                self.add(member.pointer());
            }
        }

        #[unsafe(method(minusHashTable:))]
        fn minus_hash_table(&self, other: &NSHashTable) {
            for member in hash_table_members(other) {
                self.remove(member.pointer());
            }
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSHashTable> {
            let ivars = self.ivars();
            let copy = make_hash_table(ivars.functions);
            let (table, loaded) = ivars.table.read().duplicate();
            let ours = hash_table(&copy).expect("a hash table");
            // SAFETY: the new table is only swapped in.
            let old = std::mem::replace(unsafe { ours.ivars().table.write(HASH_TABLE, ours.obj()) }, table);
            drop((old, loaded));
            copy
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let ivars = self.ivars();
            describe_table(HASH_TABLE, &ivars.table, ivars.functions, None)
        }

        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            let ivars = self.ivars();
            // SAFETY: the caller passes a valid state and buffer.
            unsafe { fast_enumerate(&ivars.table, ivars.functions, &ivars.mutations, state, buffer, len) }
        }
    }

    unsafe impl NSObjectProtocol for NSHashTableImpl {}
);

// NSMapTable

pub(crate) struct MapTableIvars {
    keys: Functions,
    values: Functions,
    table: Guarded<Table>,
    mutations: Mutations,
}

impl MapTableIvars {
    fn new(keys: Functions, values: Functions) -> Self {
        let weak = keys.memory == Memory::Weak || values.memory == Memory::Weak;
        MapTableIvars { keys, values, table: Guarded::new(Table::new(weak)), mutations: Mutations::default() }
    }
}

const MAP_TABLE: &str = "NSMapTable";

fn make_map_table(keys: Functions, values: Functions) -> Retained<NSMapTable> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSMapTable::<AnyObject, AnyObject>::alloc();
    // SAFETY: NSMapTable's class is NSMapTableImpl.
    let this = unsafe { std::mem::transmute::<Allocated<NSMapTable>, Allocated<NSMapTableImpl>>(this) };
    let this = init_map_table(this, keys, values);
    // SAFETY: NSMapTableImpl is the class registered as NSMapTable.
    unsafe { Retained::cast_unchecked(this) }
}

fn init_map_table(this: Allocated<NSMapTableImpl>, keys: Functions, values: Functions) -> Retained<NSMapTableImpl> {
    let this = this.set_ivars(MapTableIvars::new(keys, values));
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn map_table(obj: &AnyObject) -> Option<&NSMapTableImpl> {
    // SAFETY: as in `hash_table`.
    inherits(obj, &NSMAPTABLE).then(|| unsafe { &*(obj as *const AnyObject).cast::<NSMapTableImpl>() })
}

fn map_options(options: NSPointerFunctionsOptions) -> Functions {
    Functions::from_options(options, MAP_TABLE, "initWithKeyOptions:valueOptions:capacity:")
}

impl NSMapTableImpl {
    fn obj(&self) -> *const AnyObject {
        ptr::from_ref(self).cast()
    }

    fn find(&self, key: *mut c_void) -> (Option<usize>, Vec<Retained<AnyObject>>) {
        let ivars = self.ivars();
        // SAFETY: callers pass keys of the table's kind: live objects where
        // it compares objects.
        let wanted = unsafe { Wanted::new(ivars.keys, key) };
        let mut loaded = Vec::new();
        let found = ivars.table.read().find(&wanted, &mut loaded);
        (found, loaded)
    }

    fn set(&self, value: *mut c_void, key: *mut c_void) {
        let ivars = self.ivars();
        // Held before the table is taken: copying may run code.
        // SAFETY: callers pass values of the table's kind.
        let value = unsafe { Item::new(ivars.values, value) };
        // SAFETY: as in `find`.
        let wanted = unsafe { Wanted::new(ivars.keys, key) };
        let mut loaded = Vec::new();
        let found = ivars.table.read().find(&wanted, &mut loaded);
        match found {
            Some(at) => {
                // SAFETY: only the table changes; the old value is released
                // after.
                let table = unsafe { ivars.table.write(MAP_TABLE, self.obj()) };
                let old = std::mem::replace(table.value_mut(at), value);
                ivars.mutations.bump();
                drop(old);
            }
            None => {
                // SAFETY: as in `find`.
                let key = unsafe { Item::new(ivars.keys, key) };
                // SAFETY: only the table changes; what it sweeps out is
                // released after.
                let swept = unsafe { ivars.table.write(MAP_TABLE, self.obj()) }.insert(wanted.hash, key, value);
                ivars.mutations.bump();
                drop(swept);
            }
        }
        drop(loaded);
    }

    fn remove(&self, key: *mut c_void) {
        let (found, loaded) = self.find(key);
        let Some(at) = found else { return };
        let ivars = self.ivars();
        // SAFETY: only the table changes; the entry is released after.
        let removed = unsafe { ivars.table.write(MAP_TABLE, self.obj()) }.remove(at);
        ivars.mutations.bump();
        drop((removed, loaded));
    }

    /// The live entries' objects, retained.
    fn pairs(&self) -> Vec<(Retained<AnyObject>, Retained<AnyObject>)> {
        let loaded: Vec<(Loaded, Loaded)> =
            self.ivars().table.read().live().map(|e| (e.key.load(), e.value.load())).collect();
        loaded.into_iter().filter_map(|(k, v)| Some((k?, v?))).collect()
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSMapTable"]
    #[ivars = MapTableIvars]
    pub(crate) struct NSMapTableImpl;

    impl NSMapTableImpl {
        #[unsafe(method_id(mapTableWithKeyOptions:valueOptions:))]
        fn with_options(keys: NSPointerFunctionsOptions, values: NSPointerFunctionsOptions) -> Retained<NSMapTable> {
            make_map_table(map_options(keys), map_options(values))
        }

        #[unsafe(method_id(strongToStrongObjectsMapTable))]
        fn strong_to_strong() -> Retained<NSMapTable> {
            make_map_table(Functions::STRONG, Functions::STRONG)
        }

        #[unsafe(method_id(weakToStrongObjectsMapTable))]
        fn weak_to_strong() -> Retained<NSMapTable> {
            make_map_table(Functions::WEAK, Functions::STRONG)
        }

        #[unsafe(method_id(strongToWeakObjectsMapTable))]
        fn strong_to_weak() -> Retained<NSMapTable> {
            make_map_table(Functions::STRONG, Functions::WEAK)
        }

        #[unsafe(method_id(weakToWeakObjectsMapTable))]
        fn weak_to_weak() -> Retained<NSMapTable> {
            make_map_table(Functions::WEAK, Functions::WEAK)
        }

        #[unsafe(method_id(mapTableWithStrongToStrongObjects))]
        fn old_strong_to_strong() -> Retained<AnyObject> {
            util::upcast(make_map_table(Functions::STRONG, Functions::STRONG))
        }

        #[unsafe(method_id(mapTableWithWeakToStrongObjects))]
        fn old_weak_to_strong() -> Retained<AnyObject> {
            util::upcast(make_map_table(Functions::WEAK, Functions::STRONG))
        }

        #[unsafe(method_id(mapTableWithStrongToWeakObjects))]
        fn old_strong_to_weak() -> Retained<AnyObject> {
            util::upcast(make_map_table(Functions::STRONG, Functions::WEAK))
        }

        #[unsafe(method_id(mapTableWithWeakToWeakObjects))]
        fn old_weak_to_weak() -> Retained<AnyObject> {
            util::upcast(make_map_table(Functions::WEAK, Functions::WEAK))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_map_table(this, Functions::STRONG, Functions::STRONG)
        }

        /// The capacity is a hint, which Sidestep's tables don't need.
        #[unsafe(method_id(initWithKeyOptions:valueOptions:capacity:))]
        fn init_with_options(
            this: Allocated<Self>,
            keys: NSPointerFunctionsOptions,
            values: NSPointerFunctionsOptions,
            _capacity: NSUInteger,
        ) -> Retained<Self> {
            init_map_table(this, map_options(keys), map_options(values))
        }

        #[unsafe(method_id(initWithKeyPointerFunctions:valuePointerFunctions:capacity:))]
        fn init_with_pointer_functions(
            this: Allocated<Self>,
            keys: &NSPointerFunctions,
            values: &NSPointerFunctions,
            _capacity: NSUInteger,
        ) -> Retained<Self> {
            init_map_table(this, functions_of(keys), functions_of(values))
        }

        #[unsafe(method_id(keyPointerFunctions))]
        fn key_pointer_functions(&self) -> Retained<NSPointerFunctions> {
            make_pointer_functions(self.ivars().keys)
        }

        #[unsafe(method_id(valuePointerFunctions))]
        fn value_pointer_functions(&self) -> Retained<NSPointerFunctions> {
            make_pointer_functions(self.ivars().values)
        }

        #[unsafe(method(objectForKey:))]
        fn object_for_key(&self, key: *mut AnyObject) -> *mut AnyObject {
            if key.is_null() && self.ivars().keys.holds_objects() {
                return ptr::null_mut();
            }
            let (found, loaded) = self.find(key.cast());
            // Read with the table counted: handing out a weak value
            // autoreleases it, which may run code.
            let value = found.map_or(ptr::null_mut(), |at| self.ivars().table.read().entries()[at].value.handed_out());
            drop(loaded);
            value.cast()
        }

        /// Nil keys and values are ignored, as in Foundation.
        #[unsafe(method(setObject:forKey:))]
        fn set_object_for_key(&self, value: *mut AnyObject, key: *mut AnyObject) {
            let ivars = self.ivars();
            if (value.is_null() && ivars.values.holds_objects()) || (key.is_null() && ivars.keys.holds_objects()) {
                return;
            }
            self.set(value.cast(), key.cast());
        }

        #[unsafe(method(removeObjectForKey:))]
        fn remove_object_for_key(&self, key: *mut AnyObject) {
            if !key.is_null() || !self.ivars().keys.holds_objects() {
                self.remove(key.cast());
            }
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            // SAFETY: counting reads no entry's object and runs no other
            // code.
            unsafe { self.ivars().table.peek() }.live_count()
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all_objects(&self) {
            let ivars = self.ivars();
            // SAFETY: only the table changes; the entries are released after.
            let removed = unsafe { ivars.table.write(MAP_TABLE, self.obj()) }.clear();
            ivars.mutations.bump();
            drop(removed);
        }

        #[unsafe(method_id(keyEnumerator))]
        fn key_enumerator(&self) -> Retained<NSEnumerator> {
            let ivars = self.ivars();
            let snapshot = array::make(objects(&ivars.table, false));
            enumerator::make(Source::snapshot(snapshot, util::upcast(self.retain()), ivars.mutations.as_ptr()))
        }

        #[unsafe(method_id(objectEnumerator))]
        fn object_enumerator(&self) -> Option<Retained<NSEnumerator>> {
            let ivars = self.ivars();
            let snapshot = array::make(objects(&ivars.table, true));
            Some(enumerator::make(Source::snapshot(snapshot, util::upcast(self.retain()), ivars.mutations.as_ptr())))
        }

        /// Keys copied, as a dictionary takes them.
        #[unsafe(method_id(dictionaryRepresentation))]
        fn dictionary_representation(&self) -> Retained<NSDictionary> {
            let mut table = crate::table::Table::default();
            for (key, value) in self.pairs() {
                table.insert(util::copy_key(&key), value);
            }
            dictionary::make(table)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMapTable> {
            let ivars = self.ivars();
            let copy = make_map_table(ivars.keys, ivars.values);
            let (table, loaded) = ivars.table.read().duplicate();
            let ours = map_table(&copy).expect("a map table");
            // SAFETY: the new table is only swapped in.
            let old = std::mem::replace(unsafe { ours.ivars().table.write(MAP_TABLE, ours.obj()) }, table);
            drop((old, loaded));
            copy
        }

        /// The same keys, with equal values.
        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(map_table).is_some_and(|other| {
                let pairs = self.pairs();
                pairs.len() == other.ivars().table.read().live_count()
                    && pairs.iter().all(|(key, value)| {
                        let (found, loaded) = other.find(Retained::as_ptr(key).cast_mut().cast());
                        let theirs = found.and_then(|at| other.ivars().table.read().entries()[at].value.load());
                        drop(loaded);
                        theirs.is_some_and(|theirs| util::equal(value, &theirs))
                    })
            })
        }

        /// The count, as in Foundation's collections.
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            self.ivars().table.read().live_count()
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let ivars = self.ivars();
            describe_table(MAP_TABLE, &ivars.table, ivars.keys, Some(ivars.values))
        }

        /// The keys.
        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            let ivars = self.ivars();
            // SAFETY: the caller passes a valid state and buffer.
            unsafe { fast_enumerate(&ivars.table, ivars.keys, &ivars.mutations, state, buffer, len) }
        }
    }

    unsafe impl NSObjectProtocol for NSMapTableImpl {}
);

// NSPointerFunctions

pub(crate) fn make_pointer_functions(functions: Functions) -> Retained<NSPointerFunctions> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSPointerFunctions::alloc();
    // SAFETY: NSPointerFunctions's class is NSPointerFunctionsImpl.
    let this = unsafe { std::mem::transmute::<Allocated<NSPointerFunctions>, Allocated<NSPointerFunctionsImpl>>(this) };
    let this = this.set_ivars(Cell::new(functions));
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSPointerFunctionsImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSPointerFunctionsImpl is the class registered as
    // NSPointerFunctions.
    unsafe { Retained::cast_unchecked(this) }
}

define_class!(
    /// The options a collection's side was made with. Custom functions
    /// (`-setHashFunction:` and the rest) aren't supported.
    #[unsafe(super(NSObject))]
    #[name = "NSPointerFunctions"]
    #[ivars = Cell<Functions>]
    pub(crate) struct NSPointerFunctionsImpl;

    impl NSPointerFunctionsImpl {
        #[unsafe(method_id(pointerFunctionsWithOptions:))]
        fn with_options(options: NSPointerFunctionsOptions) -> Retained<NSPointerFunctions> {
            make_pointer_functions(Functions::from_options(options, "NSPointerFunctions", "initWithOptions:"))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(Cell::new(Functions::STRONG));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithOptions:))]
        fn init_with_options(this: Allocated<Self>, options: NSPointerFunctionsOptions) -> Retained<Self> {
            let functions = Functions::from_options(options, "NSPointerFunctions", "initWithOptions:");
            let this = this.set_ivars(Cell::new(functions));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(usesWeakReadAndWriteBarriers))]
        fn uses_weak_barriers(&self) -> bool {
            self.ivars().get().barrier == Barrier::Weak
        }

        /// Changes what is reported, not how pointers are held.
        #[unsafe(method(setUsesWeakReadAndWriteBarriers:))]
        fn set_uses_weak_barriers(&self, weak: bool) {
            let mut functions = self.ivars().get();
            functions.barrier = if weak { Barrier::Weak } else { Barrier::None };
            self.ivars().set(functions);
        }

        #[unsafe(method(usesStrongWriteBarrier))]
        fn uses_strong_barrier(&self) -> bool {
            self.ivars().get().barrier == Barrier::Strong
        }

        /// Changes what is reported, not how pointers are held.
        #[unsafe(method(setUsesStrongWriteBarrier:))]
        fn set_uses_strong_barrier(&self, strong: bool) {
            let mut functions = self.ivars().get();
            functions.barrier = if strong { Barrier::Strong } else { Barrier::None };
            self.ivars().set(functions);
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSPointerFunctions> {
            make_pointer_functions(self.ivars().get())
        }
    }

    unsafe impl NSObjectProtocol for NSPointerFunctionsImpl {}
);

#[cfg(test)]
mod tests {
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;

    use super::{Functions, Item, NOTHING, Table};

    fn weak_item(obj: &Retained<NSObject>) -> Item {
        // SAFETY: a live object, as weak items hold.
        unsafe { Item::new(Functions::WEAK, Retained::as_ptr(obj).cast_mut().cast()) }
    }

    #[test]
    fn dying_members_are_swept_out() {
        let mut table = Table::new(true);
        let kept: Vec<_> = (0..10).map(|_| NSObject::new()).collect();
        for (i, obj) in kept.iter().enumerate() {
            drop(table.insert(i, weak_item(obj), NOTHING));
        }
        for i in 0..10_000 {
            let obj = NSObject::new();
            drop(table.insert(100 + i, weak_item(&obj), NOTHING));
        }
        assert_eq!(table.live_count(), 10);
        assert!(table.held() <= 22, "{} entries held for 10 live", table.held());
    }
}
