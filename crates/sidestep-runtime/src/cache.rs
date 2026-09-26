//! Method caches, which make a message cheap.
//!
//! A class keeps its cache in one word: a pointer to a power-of-two array of
//! (selector, implementation) slots, with the array's index mask in the top
//! 16 bits, the way Apple's runtime packs it. A cached message then costs a
//! load of that word, a load of the slot and a compare, all plain loads. It
//! needs no lock, no memory barrier, and no check that the class is
//! initialized: a class only gets a table of its own once its `+initialize`
//! has run. Until then it has the shared empty table, which always misses,
//! and misses take the slow path that loads and initializes classes.
//!
//! Why relaxed, unlocked reads are sound:
//!
//! - Tables live in memory that has only ever held zeros and this module's
//!   atomic stores: fresh anonymous mappings from the kernel (zero-filled,
//!   exactly like `.bss`), carved up by a bump allocator and never unmapped
//!   or reused. So whatever a reader sees of a table, however stale, is a
//!   value this module stored or zero.
//! - A slot is written at most once in its life, by a writer holding `BOOK`:
//!   the implementation, then a tag, the selector XOR the implementation. A
//!   reader loads both and has a hit when tag XOR implementation is its
//!   selector. Seeing both halves of a fill gives exactly that. Seeing only
//!   the tag gives the selector XOR a code address, and seeing only the
//!   implementation gives a code address, neither of which is ever a
//!   selector's address, so a half-seen fill is a miss. There is no separate
//!   check for it on the hit path.
//! - The mask can't disagree with the table it indexes: both come from the
//!   same word.
//! - A reader holding a replaced table's word gets the implementations of
//!   before the replacement. That only happens to a message racing with a
//!   method change, which is a race on Apple's runtime too; a thread that
//!   made or synchronized with the change sees the new word.
//!
//! This is what Apple's `objc_msgSend` does with plain loads in assembly.
//! An acquire load would cost about half a nanosecond per message on arm64
//! after any release store, such as an object's release.
//!
//! Tables double as they grow (at half full, or while still sparse when a
//! new selector's first slot is taken) and are replaced wholesale only when a
//! method table changes while classes are in use, which is rare, so the
//! memory never reclaimed stays small: what a class's growth leaves behind
//! is less than its current table.

use std::collections::BTreeMap;
use std::hint::cold_path;
use std::mem::transmute;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use crate::Imp;
use crate::class::{Class, INITIALIZED};
use crate::selector::Sel;
use crate::util::lock;

#[repr(C, align(16))]
pub(crate) struct Slot {
    /// The selector XOR the implementation; zero while free.
    tag: AtomicUsize,
    imp: AtomicUsize,
}

/// The table every class starts with: one free slot.
static EMPTY: [Slot; 1] = [Slot { tag: AtomicUsize::new(0), imp: AtomicUsize::new(0) }];

/// Where the mask starts in a cache word. User-space addresses on the 64-bit
/// targets Sidestep supports fit below it; a table that doesn't is simply
/// never installed.
const MASK_SHIFT: u32 = 48;
const ADDRESS: usize = (1 << MASK_SHIFT) - 1;
/// The largest table whose mask fits in a cache word.
const MAX_SLOTS: usize = 1 << (usize::BITS - MASK_SHIFT);

const _: () = assert!(usize::BITS == 64, "Sidestep's runtime supports 64-bit targets");

/// What writers keep, under this lock.
static BOOK: Mutex<Book> = Mutex::new(Book { used: BTreeMap::new(), next: std::ptr::null_mut(), left: 0 });

struct Book {
    /// Classes with a table of their own (by address) and how many entries
    /// it holds.
    used: BTreeMap<usize, usize>,
    /// The free part of the current mapping.
    next: *mut Slot,
    left: usize,
}

// SAFETY: the pointer is into mappings that are never unmapped.
unsafe impl Send for Book {}

/// How much address space to map at a time for tables.
const CHUNK_SLOTS: usize = (2 << 20) / size_of::<Slot>();
/// Counts method table changes, so a lookup that raced one doesn't cache
/// what may be a stale implementation.
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// A class's cache word as it starts: the empty table.
pub(crate) const fn empty() -> AtomicPtr<Slot> {
    AtomicPtr::new((&raw const EMPTY).cast::<Slot>().cast_mut())
}

fn empty_word() -> *mut Slot {
    (&raw const EMPTY).cast::<Slot>().cast_mut()
}

/// The slots and index mask a cache word packs.
#[inline(always)]
fn unpack(word: *mut Slot) -> (*const Slot, usize) {
    (word.map_addr(|a| a & ADDRESS).cast_const(), word.addr() >> MASK_SHIFT)
}

/// Where a selector starts probing. Selectors are 16-byte aligned.
#[inline(always)]
fn home(sel: usize, mask: usize) -> usize {
    (sel >> 4) & mask
}

/// The cached implementation of `sel` for `cls`, found without locking.
///
/// One loop, with only the mismatch marked cold, so the common case (the
/// selector in the first slot it probes) runs straight through and a
/// collision costs a few more instructions, not a call.
#[inline(always)]
pub(crate) fn probe(cls: &Class, sel: Sel) -> Option<Imp> {
    let (slots, mask) = unpack(cls.cache.load(Ordering::Relaxed));
    let key = sel.addr();
    let mut at = home(key, mask);
    loop {
        // SAFETY: `at <= mask`, the table has `mask + 1` slots, and tables
        // are never freed.
        let slot = unsafe { &*slots.add(at) };
        let (tag, imp) = (slot.tag.load(Ordering::Relaxed), slot.imp.load(Ordering::Relaxed));
        if tag ^ imp == key {
            // SAFETY: the tag matches only a fully seen fill, so `imp` is the
            // implementation stored for `sel` (see the module comment).
            return Some(unsafe { transmute::<usize, Imp>(imp) });
        }
        cold_path();
        if tag == 0 {
            // Free (or not yet visibly filled): tables are never full, so
            // this ends the probe.
            return None;
        }
        at = (at + 1) & mask;
    }
}

/// Read before resolving a method, and handed to [`remember`].
pub(crate) fn epoch() -> u64 {
    EPOCH.load(Ordering::Acquire)
}

/// Cache that `sel` resolves to `imp` for `cls`, a resolution that started
/// at `epoch`.
pub(crate) fn remember(cls: &'static Class, sel: Sel, imp: Imp, epoch: u64) {
    if cls.instance_class().flags() & INITIALIZED == 0 {
        // A cache hit would skip +initialize.
        return;
    }
    let mut book = lock(&BOOK);
    if EPOCH.load(Ordering::Acquire) != epoch {
        // A method table changed meanwhile; `imp` may be stale.
        return;
    }
    let key = cls as *const Class as usize;
    let (mut slots, mut mask) = unpack(cls.cache.load(Ordering::Relaxed));
    let mut used = book.used.get(&key).copied();
    // At most half full, so most selectors are in the first slot they
    // probe. A table still sparse also doubles when the new selector's
    // first slot is taken: two selectors sharing a slot at one size may
    // not at the next, and a table that small costs little memory. Classes
    // use few selectors, so this keeps nearly all of them in their first
    // slot, whatever order they arrive in.
    while used.is_none_or(|n| {
        let size = mask + 1;
        (n + 1) * 2 > size || ((n + 1) * SPARSE <= size && size < SPARSE_LIMIT && taken(slots, mask, sel.addr()))
    }) {
        let size = if used.is_some() { (mask + 1) * 2 } else { 16 };
        let Some(fresh) = new_table(&mut book, size) else { return };
        let mut n = 0;
        if used.is_some() {
            for i in 0..=mask {
                // SAFETY: within the old table.
                let slot = unsafe { &*slots.add(i) };
                let (tag, imp) = (slot.tag.load(Ordering::Relaxed), slot.imp.load(Ordering::Relaxed));
                if tag != 0 && insert(fresh, size - 1, tag ^ imp, imp) {
                    n += 1;
                }
            }
        }
        let word = fresh.cast_mut().map_addr(|a| a | ((size - 1) << MASK_SHIFT));
        cls.cache.store(word, Ordering::Release);
        (slots, mask, used) = (fresh, size - 1, Some(n));
    }
    if insert(slots, mask, sel.addr(), imp as usize) {
        book.used.insert(key, used.unwrap_or(0) + 1);
    }
}

/// A table at most one entry in `SPARSE` full, below `SPARSE_LIMIT`
/// slots, grows rather than let a new selector share a first slot.
const SPARSE: usize = 8;
const SPARSE_LIMIT: usize = 1024;

/// Whether `sel`'s first slot holds another selector.
fn taken(slots: *const Slot, mask: usize, sel: usize) -> bool {
    // SAFETY: within the table.
    let slot = unsafe { &*slots.add(home(sel, mask)) };
    let tag = slot.tag.load(Ordering::Relaxed);
    tag != 0 && tag ^ slot.imp.load(Ordering::Relaxed) != sel
}

/// A table of `size` free slots, or `None` if there's no memory for it or
/// it can't be packed into a cache word.
fn new_table(book: &mut Book, size: usize) -> Option<*const Slot> {
    if size > MAX_SLOTS {
        return None;
    }
    if book.left < size {
        let slots = CHUNK_SLOTS.max(size);
        // SAFETY: a fresh private anonymous mapping, never unmapped. The
        // kernel fills it with zeros, which are free slots.
        let chunk = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                slots * size_of::<Slot>(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if chunk == libc::MAP_FAILED {
            return None;
        }
        (book.next, book.left) = (chunk.cast::<Slot>(), slots);
    }
    let table = book.next;
    // SAFETY: within the mapping.
    book.next = unsafe { table.add(size) };
    book.left -= size;
    (book.next.addr() <= ADDRESS).then_some(table.cast_const())
}

/// Fill a free slot, holding `BOOK`. Returns whether the entry is new.
fn insert(slots: *const Slot, mask: usize, sel: usize, imp: usize) -> bool {
    let mut i = home(sel, mask);
    loop {
        // SAFETY: within the table.
        let slot = unsafe { &*slots.add(i) };
        match slot.tag.load(Ordering::Relaxed) {
            0 => {
                slot.imp.store(imp, Ordering::Relaxed);
                slot.tag.store(sel ^ imp, Ordering::Release);
                return true;
            }
            tag if tag ^ slot.imp.load(Ordering::Relaxed) == sel => return false,
            _ => i = (i + 1) & mask,
        }
    }
}

/// A method table changed: any cached implementation may be stale.
pub(crate) fn flush_all() {
    let mut book = lock(&BOOK);
    EPOCH.fetch_add(1, Ordering::AcqRel);
    for &cls in book.used.keys() {
        // SAFETY: classes are never freed.
        unsafe { &*(cls as *const Class) }.cache.store(empty_word(), Ordering::Release);
    }
    book.used.clear();
}

/// Empty one class's cache: enough for a class still being built, which
/// has no subclasses yet.
pub(crate) fn flush(cls: &Class) {
    let mut book = lock(&BOOK);
    cls.cache.store(empty_word(), Ordering::Release);
    book.used.remove(&(cls as *const Class as usize));
}
