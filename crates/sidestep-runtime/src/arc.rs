//! The ARC entry points: retain and release, autorelease pools, and weak
//! references.

use std::cell::RefCell;
use std::collections::hash_map::Entry as MapEntry;
use std::ffi::c_void;
use std::mem::transmute;
use std::sync::MutexGuard;
use std::sync::atomic::{AtomicPtr, Ordering, fence};

use crate::blocks;
use crate::class::{BLOCK, CUSTOM_RR, META};
use crate::message::objc_msg_lookup;
use crate::object::{DEALLOCATING, IMMORTAL, Kind, Object, RC_ONE, WEAKLY_REFERENCED, header, isa, isa_relaxed, kind};
use crate::selector::{Sel, known};
use crate::util::{AddrMap, Sharded};

type Id = *mut Object;

/// Send a message taking no arguments.
unsafe fn send<R>(obj: Id, sel: Sel) -> R {
    // SAFETY: the caller passes a live object; the method takes no
    // arguments and returns `R`.
    unsafe {
        let imp = objc_msg_lookup(obj, sel).expect("lookup never fails");
        let imp: unsafe extern "C-unwind" fn(Id, Sel) -> R = transmute(imp);
        imp(obj, sel)
    }
}

fn has_custom_rr(obj: Id) -> bool {
    // SAFETY: callers pass live, counted objects.
    unsafe { isa(obj) }.flags() & CUSTOM_RR != 0
}

/// Whether `obj` is counted in its header the default way: not a class or
/// a block, and its class doesn't override `retain`, `release` or
/// `autorelease`. The one question every retain and release asks, in two
/// relaxed loads: the object reached this thread through synchronization,
/// and its class's flags were settled before its first instance existed.
///
/// # Safety
/// `obj` must be a live object.
#[inline(always)]
unsafe fn plainly_counted(obj: Id) -> bool {
    // SAFETY: guaranteed by the caller.
    let cls = unsafe { isa_relaxed(obj) };
    cls.flags.load(Ordering::Relaxed) & (META | BLOCK | CUSTOM_RR) == 0
}

/// Count one more reference without consulting overrides.
pub(crate) unsafe fn raw_retain(obj: Id) {
    // SAFETY: the caller passes a live, counted object.
    let h = unsafe { header(obj) };
    if h.rc.load(Ordering::Relaxed) & IMMORTAL == 0 {
        h.rc.fetch_add(RC_ONE, Ordering::Relaxed);
    }
}

/// Count one reference fewer, deallocating at zero, without consulting
/// overrides.
#[inline(always)]
pub(crate) unsafe fn raw_release(obj: Id) {
    // SAFETY: the caller passes a live, counted object.
    let h = unsafe { header(obj) };
    let mut cur = h.rc.load(Ordering::Relaxed);
    loop {
        if cur & (IMMORTAL | DEALLOCATING) != 0 {
            return;
        }
        let new = if cur < RC_ONE { cur | DEALLOCATING } else { cur - RC_ONE };
        match h.rc.compare_exchange_weak(cur, new, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => cur = actual,
        }
    }
    if cur < RC_ONE {
        // SAFETY: that was the last reference.
        unsafe { deallocate(obj) };
    }
}

/// Send `-dealloc` to an object whose last reference is gone. Kept out of
/// line so a release that isn't the last stays a few instructions.
#[inline(never)]
unsafe fn deallocate(obj: Id) {
    // Every other thread's releases happened before this.
    fence(Ordering::Acquire);
    // SAFETY: the last reference is gone; -dealloc frees the object.
    unsafe { send::<()>(obj, known().dealloc) };
}

/// Retain unless the object is already deallocating. Used by weak loads.
unsafe fn try_retain(obj: Id) -> bool {
    // SAFETY: the caller passes a live, counted object.
    let h = unsafe { header(obj) };
    let mut cur = h.rc.load(Ordering::Relaxed);
    loop {
        if cur & IMMORTAL != 0 {
            return true;
        }
        if cur & DEALLOCATING != 0 {
            return false;
        }
        match h.rc.compare_exchange_weak(cur, cur + RC_ONE, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(actual) => cur = actual,
        }
    }
}

/// The number `-retainCount` reports.
pub(crate) unsafe fn retain_count(obj: Id) -> usize {
    // SAFETY: the caller passes a live, counted object.
    let bits = unsafe { header(obj) }.rc.load(Ordering::Relaxed);
    if bits & IMMORTAL != 0 { usize::MAX } else { bits / RC_ONE + 1 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_retain(obj: Id) -> Id {
    if obj.is_null() {
        return obj;
    }
    // SAFETY: the caller passes a live object.
    if unsafe { plainly_counted(obj) } {
        // SAFETY: as above.
        unsafe { raw_retain(obj) };
        return obj;
    }
    // SAFETY: as above.
    unsafe { retain_slow(obj) }
}

#[cold]
unsafe fn retain_slow(obj: Id) -> Id {
    // SAFETY: the caller passes a live object.
    match unsafe { kind(obj) } {
        Kind::Class => obj,
        // SAFETY: as above.
        Kind::Block => unsafe { blocks::retain(obj) },
        // SAFETY: as above.
        Kind::Counted if has_custom_rr(obj) => unsafe { send(obj, known().retain) },
        Kind::Counted => {
            // SAFETY: as above.
            unsafe { raw_retain(obj) };
            obj
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_release(obj: Id) {
    if obj.is_null() {
        return;
    }
    // SAFETY: the caller passes a live object it owns a reference to.
    if unsafe { plainly_counted(obj) } {
        // SAFETY: as above.
        unsafe { raw_release(obj) };
        return;
    }
    // SAFETY: as above.
    unsafe { release_slow(obj) }
}

#[cold]
unsafe fn release_slow(obj: Id) {
    // SAFETY: the caller passes a live object it owns a reference to.
    match unsafe { kind(obj) } {
        Kind::Class => {}
        // SAFETY: as above.
        Kind::Block => unsafe { blocks::release(obj) },
        // SAFETY: as above.
        Kind::Counted if has_custom_rr(obj) => unsafe { send::<()>(obj, known().release) },
        // SAFETY: as above.
        Kind::Counted => unsafe { raw_release(obj) },
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_retainBlock(obj: Id) -> Id {
    // SAFETY: the caller passes a block or null.
    unsafe { blocks::_Block_copy(obj.cast()) }.cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_storeStrong(addr: *mut Id, value: Id) {
    // SAFETY: the caller passes a valid location and object.
    unsafe {
        let value = objc_retain(value);
        let old = addr.replace(value);
        objc_release(old);
    }
}

// Autorelease pools: one stack of pending releases per thread. A pool token
// is the stack height at push time, plus one so it is never null.
//
// The return-value handoff. A method returning an object it doesn't own
// ends with objc_autoreleaseReturnValue, and a caller keeping the object
// starts with objc_retainAutoreleasedReturnValue: an autorelease the retain
// immediately undoes. When the two are adjacent, the reference can pass
// from callee to caller without touching the count or the pool:
// objc_autoreleaseReturnValue parks the object in a thread-local slot,
// with the address it will return to, and objc_retainAutoreleasedReturnValue
// takes it back if it is called on the same object from the very next
// instruction there.
//
// That adjacency is what makes the handoff invisible. Claiming on the
// object alone isn't: a caller that didn't keep the object (it took a raw
// pointer, which the pool keeps valid) may get the same object back later
// from a method that doesn't autorelease it (`-self`, a collection's
// getter), and claiming then would take the pool's reference and free the
// object under the first pointer. Apple's runtime makes the same check on
// return addresses. It applies when the method ends by jumping to
// objc_autoreleaseReturnValue (a tail call, as optimized builds do) and
// its caller claims the result straight after the call; otherwise the
// object is simply autoreleased.
//
// A parked object is logically the newest entry of the current pool.
// Everything that could tell the difference (another autorelease, a push,
// a pop and every release a pop makes, the thread ending) first settles it
// into the stack, exactly where autoreleasing it would have put it, so its
// lifetime is the same.

/// The thread's parked object (0 if none) and the address its
/// `objc_autoreleaseReturnValue` returns to. Rust's `thread_local!` reaches
/// thread-local storage through a TLS descriptor call, which would give
/// the claim a stack frame, so on aarch64 and x86_64 the two words are
/// defined here in assembly and reached with the initial-exec sequence
/// (which the linker relaxes to a constant offset from the thread pointer
/// in an executable).
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
mod returned {
    use std::cell::Cell;

    std::arch::global_asm!(
        ".pushsection .tbss,\"awT\",%nobits",
        ".p2align 4",
        ".globl sidestep_returned",
        ".hidden sidestep_returned",
        ".type sidestep_returned, %object",
        ".size sidestep_returned, 16",
        "sidestep_returned:",
        ".zero 16",
        ".popsection",
    );

    /// This thread's two words.
    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    fn slot() -> &'static [Cell<usize>; 2] {
        let addr: usize;
        // SAFETY: computes the address of this thread's copy of the words
        // above: their offset in the static TLS block, from the GOT, plus
        // the thread pointer. Reads only the GOT entry.
        unsafe {
            std::arch::asm!(
                "adrp {addr}, :gottprel:sidestep_returned",
                "ldr {addr}, [{addr}, :gottprel_lo12:sidestep_returned]",
                "mrs {tp}, tpidr_el0",
                "add {addr}, {addr}, {tp}",
                addr = out(reg) addr,
                tp = out(reg) _,
                options(pure, readonly, nostack, preserves_flags),
            );
        }
        // SAFETY: this thread's words, which live as long as the thread;
        // the reference stays private to this module and to the calling
        // thread.
        unsafe { &*std::ptr::with_exposed_provenance::<[Cell<usize>; 2]>(addr) }
    }

    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    fn slot() -> &'static [Cell<usize>; 2] {
        let addr: usize;
        // SAFETY: as on aarch64; `fs:0` holds the thread pointer.
        unsafe {
            std::arch::asm!(
                "mov {addr}, qword ptr fs:[0]",
                "add {addr}, qword ptr [rip + sidestep_returned@GOTTPOFF]",
                addr = out(reg) addr,
                options(pure, readonly, nostack),
            );
        }
        // SAFETY: as on aarch64.
        unsafe { &*std::ptr::with_exposed_provenance::<[Cell<usize>; 2]>(addr) }
    }

    /// The parked object, or 0.
    #[inline(always)]
    pub(super) fn object() -> usize {
        slot()[0].get()
    }

    /// Where the parked object's `objc_autoreleaseReturnValue` returns.
    #[inline(always)]
    pub(super) fn returns_to() -> usize {
        slot()[1].get()
    }

    /// Take the parked object out, leaving 0.
    #[inline(always)]
    pub(super) fn take() -> usize {
        slot()[0].replace(0)
    }

    /// Park `obj`, returning to `to`; gives back the object parked before.
    #[inline(always)]
    pub(super) fn park(obj: usize, to: usize) -> usize {
        let slot = slot();
        slot[1].set(to);
        slot[0].replace(obj)
    }
}

/// Elsewhere nothing is parked: without return addresses, there is no
/// telling a handoff from a stale claim.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
mod returned {
    pub(super) fn take() -> usize {
        0
    }
}

struct Pool {
    stack: RefCell<Vec<usize>>,
}

impl Pool {
    /// Move a parked object into the stack.
    #[inline]
    fn settle(&self) {
        let parked = returned::take();
        if parked != 0 {
            self.stack.borrow_mut().push(parked);
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.settle();
        let stack = self.stack.get_mut();
        while let Some(obj) = stack.pop() {
            // SAFETY: the pool owns one reference to each entry. Objects
            // autoreleased from here on can't be pooled and are leaked.
            unsafe { objc_release(obj as Id) };
        }
    }
}

thread_local!(static POOL: Pool = const { Pool { stack: RefCell::new(Vec::new()) } });

/// Add `obj` to the current pool, doing the counting part of -autorelease.
pub(crate) fn pool_add(obj: Id) {
    let _ = POOL.try_with(|pool| {
        pool.settle();
        pool.stack.borrow_mut().push(obj as usize);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn objc_autoreleasePoolPush() -> *mut c_void {
    let height = POOL
        .try_with(|pool| {
            pool.settle();
            pool.stack.borrow().len()
        })
        .unwrap_or(0);
    (height + 1) as *mut c_void
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn objc_autoreleasePoolPop(token: *mut c_void) {
    let height = (token as usize).saturating_sub(1);
    let _ = POOL.try_with(|pool| {
        loop {
            // Settled every time round: a -dealloc run by the last release
            // may have parked an object its caller didn't keep, which
            // belongs to this pool like anything else autoreleased while it
            // drains. The stack isn't borrowed while an object is released:
            // its -dealloc may autorelease more.
            let next = {
                pool.settle();
                let mut stack = pool.stack.borrow_mut();
                if stack.len() > height { stack.pop() } else { None }
            };
            let Some(obj) = next else { break };
            // SAFETY: the pool owned this reference.
            unsafe { objc_release(obj as Id) };
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_autorelease(obj: Id) -> Id {
    if obj.is_null() {
        return obj;
    }
    // SAFETY: the caller passes a live object it owns a reference to.
    if unsafe { plainly_counted(obj) } {
        pool_add(obj);
        return obj;
    }
    // SAFETY: as above.
    unsafe { autorelease_slow(obj) }
}

#[cold]
unsafe fn autorelease_slow(obj: Id) -> Id {
    // SAFETY: the caller passes a live object it owns a reference to.
    match unsafe { kind(obj) } {
        Kind::Class => obj,
        // SAFETY: as above.
        Kind::Counted if has_custom_rr(obj) => unsafe { send(obj, known().autorelease) },
        _ => {
            pool_add(obj);
            obj
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_retainAutorelease(obj: Id) -> Id {
    // SAFETY: forwarded contract.
    unsafe { objc_autorelease(objc_retain(obj)) }
}

// The three handoff entry points pass their return address on to the Rust
// functions below: a jump, so the stub leaves no frame behind.
#[cfg(target_arch = "aarch64")]
std::arch::global_asm!(
    ".text",
    ".p2align 2",
    ".globl objc_autoreleaseReturnValue",
    ".type objc_autoreleaseReturnValue, %function",
    "objc_autoreleaseReturnValue:",
    ".cfi_startproc",
    "hint #34",
    "mov x1, x30",
    "b {autorelease_return_value}",
    ".cfi_endproc",
    ".size objc_autoreleaseReturnValue, . - objc_autoreleaseReturnValue",
    ".globl objc_retainAutoreleaseReturnValue",
    ".type objc_retainAutoreleaseReturnValue, %function",
    "objc_retainAutoreleaseReturnValue:",
    ".cfi_startproc",
    "hint #34",
    "mov x1, x30",
    "b {retain_autorelease_return_value}",
    ".cfi_endproc",
    ".size objc_retainAutoreleaseReturnValue, . - objc_retainAutoreleaseReturnValue",
    ".globl objc_retainAutoreleasedReturnValue",
    ".type objc_retainAutoreleasedReturnValue, %function",
    "objc_retainAutoreleasedReturnValue:",
    ".cfi_startproc",
    "hint #34",
    "mov x1, x30",
    "b {retain_autoreleased_return_value}",
    ".cfi_endproc",
    ".size objc_retainAutoreleasedReturnValue, . - objc_retainAutoreleasedReturnValue",
    autorelease_return_value = sym autorelease_return_value,
    retain_autorelease_return_value = sym retain_autorelease_return_value,
    retain_autoreleased_return_value = sym retain_autoreleased_return_value,
);

#[cfg(target_arch = "x86_64")]
std::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl objc_autoreleaseReturnValue",
    ".type objc_autoreleaseReturnValue, @function",
    "objc_autoreleaseReturnValue:",
    ".cfi_startproc",
    "mov rsi, qword ptr [rsp]",
    "jmp {autorelease_return_value}",
    ".cfi_endproc",
    ".size objc_autoreleaseReturnValue, . - objc_autoreleaseReturnValue",
    ".p2align 4",
    ".globl objc_retainAutoreleaseReturnValue",
    ".type objc_retainAutoreleaseReturnValue, @function",
    "objc_retainAutoreleaseReturnValue:",
    ".cfi_startproc",
    "mov rsi, qword ptr [rsp]",
    "jmp {retain_autorelease_return_value}",
    ".cfi_endproc",
    ".size objc_retainAutoreleaseReturnValue, . - objc_retainAutoreleaseReturnValue",
    ".p2align 4",
    ".globl objc_retainAutoreleasedReturnValue",
    ".type objc_retainAutoreleasedReturnValue, @function",
    "objc_retainAutoreleasedReturnValue:",
    ".cfi_startproc",
    "mov rsi, qword ptr [rsp]",
    "jmp {retain_autoreleased_return_value}",
    ".cfi_endproc",
    ".size objc_retainAutoreleasedReturnValue, . - objc_retainAutoreleasedReturnValue",
    autorelease_return_value = sym autorelease_return_value,
    retain_autorelease_return_value = sym retain_autorelease_return_value,
    retain_autoreleased_return_value = sym retain_autoreleased_return_value,
);

/// Whether a claim returning to `claim` immediately follows the return to
/// `park`: the instruction there is the call to the claim. On x86_64 a move
/// of the result into the first argument register comes first, and the
/// call is direct or through the GOT.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
#[inline(always)]
fn adjacent(park: usize, claim: usize) -> bool {
    #[cfg(target_arch = "aarch64")]
    return claim.wrapping_sub(park) == 4;
    #[cfg(target_arch = "x86_64")]
    return matches!(claim.wrapping_sub(park), 8 | 9);
}

/// `objc_autoreleaseReturnValue`, returning to `to`: parks `obj` for the
/// caller to claim; see the handoff above. Objects with their own
/// `-autorelease` get it sent, as `objc_autorelease` would.
///
/// # Safety
/// `obj` is null or a live object the caller owns a reference to.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
unsafe extern "C-unwind" fn autorelease_return_value(obj: Id, to: usize) -> Id {
    // SAFETY: guaranteed by the caller.
    if !obj.is_null() && unsafe { plainly_counted(obj) } {
        // Through the pool, so it is there to settle the object when the
        // thread ends.
        let parked = POOL.try_with(|pool| {
            let previous = returned::park(obj as usize, to);
            if previous != 0 {
                pool.stack.borrow_mut().push(previous);
            }
        });
        if parked.is_ok() {
            return obj;
        }
    }
    // SAFETY: forwarded contract.
    unsafe { objc_autorelease(obj) }
}

/// `objc_retainAutoreleaseReturnValue`, returning to `to`.
///
/// # Safety
/// `obj` is null or a live object.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
unsafe extern "C-unwind" fn retain_autorelease_return_value(obj: Id, to: usize) -> Id {
    // SAFETY: forwarded contract.
    unsafe { autorelease_return_value(objc_retain(obj), to) }
}

/// `objc_retainAutoreleasedReturnValue`, returning to `to`: claims `obj`
/// if it was parked by the return this call immediately follows, taking
/// over the parked reference; otherwise retains it.
///
/// # Safety
/// `obj` is null or a live object.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
unsafe extern "C-unwind" fn retain_autoreleased_return_value(obj: Id, to: usize) -> Id {
    if !obj.is_null() && returned::object() == obj as usize && adjacent(returned::returns_to(), to) {
        returned::take();
        return obj;
    }
    // SAFETY: forwarded contract.
    unsafe { objc_retain(obj) }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_autoreleaseReturnValue(obj: Id) -> Id {
    // SAFETY: forwarded contract.
    unsafe { objc_autorelease(obj) }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_retainAutoreleaseReturnValue(obj: Id) -> Id {
    // SAFETY: forwarded contract.
    unsafe { objc_autorelease(objc_retain(obj)) }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_retainAutoreleasedReturnValue(obj: Id) -> Id {
    // SAFETY: forwarded contract.
    unsafe { objc_retain(obj) }
}

// Weak references: a table from each object to the locations that weakly
// refer to it, sharded by object address. Deallocation zeroes them, and so
// does `_Block_release` freeing a heap block. Classes, stack and global
// blocks and immortal objects are never freed by the runtime, so weak
// references to them are stored untracked.
//
// Weak locations are only read and written here, and atomically: a load
// reads the location before it knows which shard to lock, while a store or
// a deallocation changes it holding that shard's lock. So a load locks the
// shard of the object it read and checks the location still holds it.
// Holding that lock keeps the object's memory alive, because freeing it
// first zeroes its weak references under the same lock.

/// The locations weakly referring to one object: nearly always one, which
/// needs no allocation.
enum Locations {
    One(usize),
    Many(Vec<usize>),
}

static WEAK: Sharded<Locations> = Sharded::new();

/// A weak location, which the runtime reads and writes atomically.
///
/// # Safety
/// `location` must be valid and aligned for as long as the result is used.
unsafe fn weak_slot<'a>(location: *mut Id) -> &'a AtomicPtr<Object> {
    // SAFETY: guaranteed by the caller; every access to weak locations is
    // atomic.
    unsafe { AtomicPtr::from_ptr(location) }
}

/// How weak references to an object are kept.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tracking {
    /// Never freed by the runtime: stored as is.
    Untracked,
    /// Counted in its header.
    Object,
    /// A heap block, counted in its flags.
    HeapBlock,
}

/// # Safety
/// `obj` must be a live object.
unsafe fn tracking(obj: Id) -> Tracking {
    // SAFETY: guaranteed by the caller.
    match unsafe { kind(obj) } {
        // SAFETY: as above.
        Kind::Counted if unsafe { header(obj) }.rc.load(Ordering::Relaxed) & IMMORTAL == 0 => Tracking::Object,
        // SAFETY: as above.
        Kind::Block if unsafe { blocks::is_heap(obj) } => Tracking::HeapBlock,
        _ => Tracking::Untracked,
    }
}

fn register(table: &mut AddrMap<Locations>, obj: usize, location: usize) {
    match table.entry(obj) {
        MapEntry::Vacant(entry) => {
            entry.insert(Locations::One(location));
        }
        MapEntry::Occupied(mut entry) => match entry.get_mut() {
            Locations::One(first) => {
                let first = *first;
                entry.insert(Locations::Many(vec![first, location]));
            }
            Locations::Many(all) => all.push(location),
        },
    }
}

fn unregister(table: &mut AddrMap<Locations>, obj: usize, location: usize) {
    let MapEntry::Occupied(mut entry) = table.entry(obj) else { return };
    let emptied = match entry.get_mut() {
        Locations::One(only) => *only == location,
        Locations::Many(all) => {
            if let Some(i) = all.iter().position(|&l| l == location) {
                all.swap_remove(i);
            }
            all.is_empty()
        }
    };
    if emptied {
        entry.remove();
    }
}

/// Zero every weak reference to `obj`, which is being freed.
pub(crate) fn clear_weak(obj: Id) {
    let zero = |location: usize| {
        // SAFETY: registered locations stay valid until objc_destroyWeak.
        let slot = unsafe { weak_slot(location as *mut Id) };
        let _ = slot.compare_exchange(obj, std::ptr::null_mut(), Ordering::Relaxed, Ordering::Relaxed);
    };
    // Zeroed under the lock: a load that read `obj` from one of these
    // locations is waiting for it, and must find the location changed.
    let mut shard = WEAK.lock(obj as usize);
    match shard.remove(&(obj as usize)) {
        Some(Locations::One(location)) => zero(location),
        Some(Locations::Many(all)) => all.into_iter().for_each(zero),
        None => {}
    }
}

/// The shards two objects' entries live in, locked in index order so two
/// threads locking the same pair can't deadlock.
struct ShardPair<'a> {
    first: Option<(usize, MutexGuard<'a, AddrMap<Locations>>)>,
    second: Option<(usize, MutexGuard<'a, AddrMap<Locations>>)>,
}

impl<'a> ShardPair<'a> {
    fn lock(a: Option<usize>, b: Option<usize>) -> Self {
        let index = |obj: usize| Sharded::<Locations>::index(obj);
        let (low, high) = match (a.map(index), b.map(index)) {
            (Some(x), Some(y)) if x != y => (Some(x.min(y)), Some(x.max(y))),
            (Some(x), _) | (None, Some(x)) => (Some(x), None),
            (None, None) => (None, None),
        };
        ShardPair { first: low.map(|i| (i, WEAK.lock_index(i))), second: high.map(|i| (i, WEAK.lock_index(i))) }
    }

    /// The table holding `obj`, which must be one of the pair.
    fn table(&mut self, obj: usize) -> &mut AddrMap<Locations> {
        let index = Sharded::<Locations>::index(obj);
        match (&mut self.first, &mut self.second) {
            (Some((i, table)), _) if *i == index => table,
            (_, Some((_, table))) => table,
            _ => unreachable!("the shard of an object in the pair is locked"),
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_storeWeak(location: *mut Id, value: Id) -> Id {
    // SAFETY: the caller passes a valid location holding null or a weakly
    // stored object.
    let slot = unsafe { weak_slot(location) };
    // SAFETY: the caller passes a live object, which it keeps alive, or
    // null.
    let tracking = if value.is_null() { Tracking::Untracked } else { unsafe { tracking(value) } };
    let tracked = tracking != Tracking::Untracked;
    loop {
        let old = slot.load(Ordering::Relaxed);
        let mut shards = ShardPair::lock((!old.is_null()).then_some(old as usize), tracked.then_some(value as usize));
        if slot.load(Ordering::Relaxed) != old {
            // Another thread changed the location first.
            continue;
        }
        if !old.is_null() {
            unregister(shards.table(old as usize), old as usize, location as usize);
        }
        // SAFETY: as above.
        let stored = match tracking {
            Tracking::Untracked => value,
            Tracking::Object if unsafe { header(value) }.rc.load(Ordering::Acquire) & DEALLOCATING != 0 => {
                std::ptr::null_mut()
            }
            Tracking::HeapBlock if unsafe { blocks::is_released(value) } => std::ptr::null_mut(),
            Tracking::Object => {
                // SAFETY: as above.
                let rc = unsafe { &header(value).rc };
                if rc.load(Ordering::Relaxed) & WEAKLY_REFERENCED == 0 {
                    rc.fetch_or(WEAKLY_REFERENCED, Ordering::Relaxed);
                }
                register(shards.table(value as usize), value as usize, location as usize);
                value
            }
            Tracking::HeapBlock => {
                // SAFETY: as above.
                unsafe { blocks::mark(value, WEAKLY_REFERENCED) };
                register(shards.table(value as usize), value as usize, location as usize);
                value
            }
        };
        slot.store(stored, Ordering::Relaxed);
        return stored;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_initWeak(location: *mut Id, value: Id) -> Id {
    // SAFETY: the caller passes a valid, uninitialized location.
    unsafe {
        location.write(std::ptr::null_mut());
        objc_storeWeak(location, value)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_destroyWeak(location: *mut Id) {
    // SAFETY: forwarded contract.
    unsafe { objc_storeWeak(location, std::ptr::null_mut()) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_loadWeakRetained(location: *mut Id) -> Id {
    // SAFETY: the caller passes a valid weak location.
    let slot = unsafe { weak_slot(location) };
    loop {
        let obj = slot.load(Ordering::Relaxed);
        if obj.is_null() {
            return obj;
        }
        let _shard = WEAK.lock(obj as usize);
        if slot.load(Ordering::Relaxed) != obj {
            continue;
        }
        // SAFETY: the location still holds `obj`, so it isn't freed yet,
        // and can't be while this thread holds its shard's lock.
        unsafe {
            let retained = match tracking(obj) {
                Tracking::Untracked => return objc_retain(obj),
                Tracking::Object => try_retain(obj),
                Tracking::HeapBlock => blocks::try_retain(obj),
            };
            return if retained { obj } else { std::ptr::null_mut() };
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_loadWeak(location: *mut Id) -> Id {
    // SAFETY: forwarded contract.
    unsafe { objc_autorelease(objc_loadWeakRetained(location)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_copyWeak(to: *mut Id, from: *mut Id) {
    // SAFETY: forwarded contract.
    unsafe {
        let obj = objc_loadWeakRetained(from);
        objc_initWeak(to, obj);
        objc_release(obj);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_moveWeak(to: *mut Id, from: *mut Id) {
    // SAFETY: forwarded contract.
    unsafe {
        objc_copyWeak(to, from);
        objc_destroyWeak(from);
    }
}
