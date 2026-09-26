//! Selectors are interned by name. A `SEL` points to a [`Selector`], laid out
//! like libobjc2's `struct objc_selector`, and equal names always give the
//! same pointer, so selectors compare by address.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::mem::MaybeUninit;
use std::sync::{LazyLock, Mutex, RwLock};

use crate::util::{Shared, leak_cstr, lock};
use crate::{Bool, NO, YES};

/// An interned selector. Laid out like libobjc2's, aligned to 16 bytes:
/// method caches index by `sel >> 4`.
#[repr(C, align(16))]
pub struct Selector {
    name: *const c_char,
    /// libobjc2 supports typed selectors; Sidestep's are all untyped.
    types: *const c_char,
}

// SAFETY: a selector is immutable, and its name is a never-freed C string.
unsafe impl Sync for Selector {}

pub(crate) type Sel = *const Selector;

/// The runtime's own selectors, in the order they sit at the start of the
/// arena; see [`Known`].
const BUILTIN: [&CStr; 14] = [
    c"dealloc",
    c"initialize",
    c"alloc",
    c"allocWithZone:",
    c"init",
    c"retain",
    c"release",
    c"autorelease",
    c"retainCount",
    c"copy",
    c"resolveInstanceMethod:",
    c"resolveClassMethod:",
    c"forwardingTargetForSelector:",
    c"doesNotRecognizeSelector:",
];

static TABLE: LazyLock<RwLock<HashMap<&'static CStr, Shared<Selector>>>> =
    LazyLock::new(|| RwLock::new((0..BUILTIN.len()).map(|i| (BUILTIN[i], Shared(builtin(i)))).collect()));

/// How many selectors an arena chunk holds.
const CHUNK: usize = 1024;

/// The arena's first chunk, in static memory: the runtime's own selectors,
/// then room for the program's. Keeping the built-in ones in the arena,
/// rather than apart, keeps them side by side with the rest, as the method
/// cache's indexing assumes.
#[repr(transparent)]
struct FirstChunk([UnsafeCell<MaybeUninit<Selector>>; CHUNK]);

// SAFETY: a slot is written once, holding the arena's lock, before the
// selector in it is published through the table's lock; then it is only
// read.
unsafe impl Sync for FirstChunk {}

static FIRST: FirstChunk = FirstChunk(first_chunk());

const fn first_chunk() -> [UnsafeCell<MaybeUninit<Selector>>; CHUNK] {
    let mut chunk = [const { UnsafeCell::new(MaybeUninit::uninit()) }; CHUNK];
    let mut i = 0;
    while i < BUILTIN.len() {
        let sel = Selector { name: BUILTIN[i].as_ptr(), types: std::ptr::null() };
        chunk[i] = UnsafeCell::new(MaybeUninit::new(sel));
        i += 1;
    }
    chunk
}

/// The `i`th built-in selector.
const fn builtin(i: usize) -> Sel {
    (&raw const FIRST).cast::<Selector>().wrapping_add(i)
}

/// Where new selectors go: side by side, so selectors registered together
/// have consecutive addresses and land in distinct cache slots. Never freed.
struct Arena {
    next: *mut Selector,
    left: usize,
    /// Every chunk after the first, by address range, for [`is_selector`].
    chunks: Vec<std::ops::Range<usize>>,
}

// SAFETY: the pointer is into the first chunk or leaked memory, used under
// the lock.
unsafe impl Send for Arena {}

static ARENA: Mutex<Arena> =
    Mutex::new(Arena { next: builtin(BUILTIN.len()).cast_mut(), left: CHUNK - BUILTIN.len(), chunks: Vec::new() });

fn allocate(selector: Selector) -> &'static Selector {
    let mut arena = lock(&ARENA);
    if arena.left == 0 {
        let chunk: &'static mut [MaybeUninit<Selector>] = Box::leak(Box::new_uninit_slice(CHUNK));
        let range = chunk.as_ptr_range();
        arena.chunks.push(range.start.addr()..range.end.addr());
        (arena.next, arena.left) = (chunk.as_mut_ptr().cast(), CHUNK);
    }
    let sel = arena.next;
    // SAFETY: `sel` is the next unused slot of the first chunk (inside an
    // `UnsafeCell`) or of a leaked one.
    unsafe {
        sel.write(selector);
        arena.next = sel.add(1);
    }
    arena.left -= 1;
    // SAFETY: just written, and never freed or moved.
    unsafe { &*sel }
}

/// Whether `addr` is a selector's address. Selectors live only in the
/// arena's chunks, so this tells a selector from any other pointer (an
/// object, say) without reading through it.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub(crate) fn is_selector(addr: usize) -> bool {
    let first = builtin(0).addr()..builtin(CHUNK).addr();
    addr.is_multiple_of(align_of::<Selector>())
        && (first.contains(&addr) || lock(&ARENA).chunks.iter().any(|chunk| chunk.contains(&addr)))
}

pub(crate) fn register(name: &CStr) -> Sel {
    if let Some(sel) = TABLE.read().unwrap().get(name) {
        return sel.0;
    }
    let mut table = TABLE.write().unwrap();
    if let Some(sel) = table.get(name) {
        return sel.0;
    }
    let name = leak_cstr(name);
    let sel = allocate(Selector { name: name.as_ptr(), types: std::ptr::null() });
    table.insert(name, Shared(sel));
    sel
}

/// The name of `sel`, which must be null or a selector from this module.
pub(crate) unsafe fn name<'a>(sel: Sel) -> &'a CStr {
    if sel.is_null() {
        return c"<null selector>";
    }
    // SAFETY: interned selectors hold leaked C strings.
    unsafe { CStr::from_ptr((*sel).name) }
}

/// Selectors the runtime itself sends. They sit at fixed places in static
/// memory, interned before any other selector, so sending one costs no
/// lookup.
pub(crate) struct Known {
    pub dealloc: Sel,
    pub initialize: Sel,
    pub alloc: Sel,
    pub alloc_with_zone: Sel,
    pub init: Sel,
    pub retain: Sel,
    pub release: Sel,
    pub autorelease: Sel,
    pub retain_count: Sel,
    pub copy: Sel,
    pub resolve_instance_method: Sel,
    pub resolve_class_method: Sel,
    pub forwarding_target: Sel,
    pub does_not_recognize: Sel,
}
// SAFETY: selectors are immutable and never freed.
unsafe impl Send for Known {}
// SAFETY: as above.
unsafe impl Sync for Known {}

static KNOWN: Known = Known {
    dealloc: builtin(0),
    initialize: builtin(1),
    alloc: builtin(2),
    alloc_with_zone: builtin(3),
    init: builtin(4),
    retain: builtin(5),
    release: builtin(6),
    autorelease: builtin(7),
    retain_count: builtin(8),
    copy: builtin(9),
    resolve_instance_method: builtin(10),
    resolve_class_method: builtin(11),
    forwarding_target: builtin(12),
    does_not_recognize: builtin(13),
};

#[inline(always)]
pub(crate) fn known() -> &'static Known {
    &KNOWN
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sel_registerName(name: *const c_char) -> Sel {
    if name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes a C string.
    register(unsafe { CStr::from_ptr(name) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sel_getUid(name: *const c_char) -> Sel {
    // SAFETY: same contract.
    unsafe { sel_registerName(name) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sel_getName(sel: Sel) -> *const c_char {
    // SAFETY: the caller passes a selector or null.
    unsafe { name(sel) }.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn sel_isEqual(lhs: Sel, rhs: Sel) -> Bool {
    if lhs == rhs { YES } else { NO }
}
