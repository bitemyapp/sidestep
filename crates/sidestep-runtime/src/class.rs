//! Classes: layout, the name registry, construction, loading of static
//! shells, and `+initialize`.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int};
use std::mem::transmute;
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock, RwLock};

use crate::ivar::Ivar;
use crate::method::Method;
use crate::object::Object;
use crate::protocol::Protocol;
use crate::selector::{Sel, known};
use crate::util::{Shared, cstr_or, leak_cstr, lock, malloc_array, with_load_lock};
use crate::{Bool, Imp, NO, YES};

pub(crate) const META: u32 = 1 << 0;
pub(crate) const LOADED: u32 = 1 << 1;
pub(crate) const CLAIMED: u32 = 1 << 2;
pub(crate) const ROOT: u32 = 1 << 3;
/// Instances override `retain`, `release` or `autorelease`, so the ARC
/// entry points must send those messages instead of counting directly.
pub(crate) const CUSTOM_RR: u32 = 1 << 4;
pub(crate) const INITIALIZED: u32 = 1 << 5;
pub(crate) const INITIALIZING: u32 = 1 << 6;
pub(crate) const SHELL: u32 = 1 << 7;
/// One of the block classes, whose instances follow the blocks ABI rather
/// than the object header layout.
pub(crate) const BLOCK: u32 = 1 << 8;
/// The class overrides `+alloc` or `+allocWithZone:`, so `+new` must send
/// `+alloc` instead of allocating directly.
pub(crate) const CUSTOM_ALLOC: u32 = 1 << 9;
/// Flags a class passes on to its subclasses.
pub(crate) const INHERITED: u32 = CUSTOM_RR | CUSTOM_ALLOC;

/// A class or metaclass.
///
/// Every class object starts with its `isa`, like any object, so messages can
/// be sent to it. The remaining fields are private to the runtime.
#[repr(C)]
pub struct Class {
    pub(crate) isa: AtomicPtr<Class>,
    pub(crate) superclass: AtomicPtr<Class>,
    pub(crate) name: *const c_char,
    pub(crate) flags: AtomicU32,
    /// A class's metaclass, or a metaclass's class.
    pub(crate) peer: AtomicPtr<Class>,
    /// Defines a static shell on first use. `None` for metaclasses and for
    /// classes created at run time.
    pub(crate) loader: Option<fn()>,
    /// The method cache, read on every message without locking; see
    /// `cache`.
    pub(crate) cache: AtomicPtr<crate::cache::Slot>,
    /// The size and alignment of an instance, not counting its header. Set
    /// while the class is built and read on every allocation, so they live
    /// here rather than behind `rt`.
    pub(crate) instance_size: AtomicUsize,
    pub(crate) instance_align: AtomicUsize,
    /// An instance's allocation, header included, worked out when the class
    /// is registered: its size in the high half, the object's offset into
    /// it (also its alignment) in the low half. Zero until then.
    pub(crate) alloc_layout: AtomicU64,
    pub(crate) rt: OnceLock<ClassRt>,
}

// SAFETY: every mutable field is atomic or behind a lock; `name` points to
// immutable, never-freed memory.
unsafe impl Sync for Class {}
// SAFETY: as above.
unsafe impl Send for Class {}

#[derive(Default)]
pub(crate) struct MethodTable {
    pub(crate) by_sel: HashMap<usize, Shared<Method>>,
    pub(crate) order: Vec<Shared<Method>>,
}

/// Runtime data behind a class, created on first use.
#[derive(Default)]
pub(crate) struct ClassRt {
    pub(crate) methods: RwLock<MethodTable>,
    pub(crate) ivars: RwLock<Vec<Shared<Ivar>>>,
    pub(crate) protocols: RwLock<Vec<Shared<Protocol>>>,
    pub(crate) properties: RwLock<Vec<Shared<crate::property::Property>>>,
    pub(crate) version: AtomicI32,
}

impl Class {
    /// A class shell for [`static_class!`](crate::static_class). Not for
    /// direct use.
    #[doc(hidden)]
    pub const fn shell(meta: &'static Class, name: &'static str, load: fn()) -> Class {
        Class::shell_with_flags(meta, name, load, 0)
    }

    pub(crate) const fn shell_with_flags(meta: &'static Class, name: &'static str, load: fn(), flags: u32) -> Class {
        let meta = (meta as *const Class).cast_mut();
        Class {
            isa: AtomicPtr::new(meta),
            superclass: AtomicPtr::new(std::ptr::null_mut()),
            name: name.as_ptr().cast(),
            flags: AtomicU32::new(SHELL | flags),
            peer: AtomicPtr::new(meta),
            loader: Some(load),
            cache: crate::cache::empty(),
            instance_size: AtomicUsize::new(0),
            instance_align: AtomicUsize::new(0),
            alloc_layout: AtomicU64::new(0),
            rt: OnceLock::new(),
        }
    }

    /// A metaclass shell for [`static_class!`](crate::static_class). Not
    /// for direct use.
    ///
    /// Its `isa` is NSObject's metaclass from the start, which is what
    /// loading sets for every class rooted at NSObject (NSObject's own
    /// included), so a metaclass found through the class list before its
    /// class loads is a valid object. A shell for another root class gets
    /// its own metaclass when it loads.
    #[doc(hidden)]
    pub const fn meta_shell(class: &'static Class, name: &'static str) -> Class {
        Class {
            isa: AtomicPtr::new((&crate::nsobject::NSOBJECT_METACLASS as *const Class).cast_mut()),
            superclass: AtomicPtr::new(std::ptr::null_mut()),
            name: name.as_ptr().cast(),
            flags: AtomicU32::new(SHELL | META),
            peer: AtomicPtr::new((class as *const Class).cast_mut()),
            loader: None,
            cache: crate::cache::empty(),
            instance_size: AtomicUsize::new(0),
            instance_align: AtomicUsize::new(0),
            alloc_layout: AtomicU64::new(0),
            rt: OnceLock::new(),
        }
    }

    fn dynamic(name: &'static CStr, flags: u32) -> Class {
        Class {
            isa: AtomicPtr::new(std::ptr::null_mut()),
            superclass: AtomicPtr::new(std::ptr::null_mut()),
            name: name.as_ptr(),
            flags: AtomicU32::new(flags),
            peer: AtomicPtr::new(std::ptr::null_mut()),
            loader: None,
            cache: crate::cache::empty(),
            instance_size: AtomicUsize::new(0),
            instance_align: AtomicUsize::new(0),
            alloc_layout: AtomicU64::new(0),
            rt: OnceLock::new(),
        }
    }

    pub(crate) fn rt(&self) -> &ClassRt {
        self.rt.get_or_init(ClassRt::default)
    }

    pub(crate) fn flags(&self) -> u32 {
        self.flags.load(Ordering::Acquire)
    }

    pub(crate) fn is_meta(&self) -> bool {
        self.flags() & META != 0
    }

    pub(crate) fn is_loaded(&self) -> bool {
        self.flags() & LOADED != 0
    }

    pub(crate) fn name(&self) -> &'static CStr {
        // SAFETY: names are static or leaked C strings.
        unsafe { CStr::from_ptr(self.name) }
    }

    pub(crate) fn superclass(&self) -> Option<&'static Class> {
        // SAFETY: classes are never freed.
        unsafe { self.superclass.load(Ordering::Acquire).as_ref() }
    }

    /// The class's `isa`: its metaclass, or for a metaclass the root
    /// metaclass.
    pub(crate) fn metaclass(&self) -> &'static Class {
        // SAFETY: classes are never freed; `isa` is set before a class is
        // used.
        unsafe { &*self.isa.load(Ordering::Acquire) }
    }

    pub(crate) fn peer(&self) -> &'static Class {
        // SAFETY: as above.
        unsafe { &*self.peer.load(Ordering::Acquire) }
    }

    /// The non-meta class of a class or metaclass.
    pub(crate) fn instance_class(&'static self) -> &'static Class {
        if self.is_meta() { self.peer() } else { self }
    }

    /// Relaxed: they are written while the class is built, and read once it
    /// is loaded, which the reader established with an acquire load of the
    /// flags (`ensure_loaded`).
    pub(crate) fn instance_size(&self) -> usize {
        self.instance_size.load(Ordering::Relaxed)
    }

    pub(crate) fn instance_align(&self) -> usize {
        self.instance_align.load(Ordering::Relaxed)
    }

    pub(crate) fn is_subclass_of(&self, other: &Class) -> bool {
        let mut cls = Some(self);
        while let Some(c) = cls {
            if std::ptr::eq(c, other) {
                return true;
            }
            cls = c.superclass();
        }
        false
    }
}

/// One entry of the program's list of static shells; see [`linked`]. Not
/// for direct use.
#[doc(hidden)]
#[repr(transparent)]
pub struct LinkedClass(pub &'static Class);

/// Every static shell linked into the program, loaded or not.
///
/// [`static_class!`](crate::static_class) puts a pointer to each shell in
/// the `sidestep_classes` section. The linker concatenates that section
/// from every object it links and, because the name is a valid C
/// identifier, defines `__start_sidestep_classes` and
/// `__stop_sidestep_classes` around the result, with no constructor
/// involved. `#[used]` keeps the entries through garbage collection of
/// sections.
///
/// Which objects are linked decides which classes are listed. rustc links
/// into every executable and shared library an object referring to each
/// symbol an upstream crate exports under a fixed name, and every shell is
/// exported as `._OBJC_CLASS_<Name>`. So linking a framework crate at all
/// (which `use sidestep as _;` does) links every shell it defines, and its
/// entries with it, whether or not the program names the class.
fn linked() -> &'static [LinkedClass] {
    unsafe extern "Rust" {
        #[link_name = "__start_sidestep_classes"]
        static START: LinkedClass;
        #[link_name = "__stop_sidestep_classes"]
        static STOP: LinkedClass;
    }
    let (start, stop) = (&raw const START, &raw const STOP);
    let len = (stop.addr() - start.addr()) / size_of::<LinkedClass>();
    // SAFETY: the linker places the section's contents, pointer-sized and
    // pointer-aligned entries written by `static_class!`, contiguously
    // between the two symbols. The runtime's own shells (NSObject, the
    // block classes) guarantee the section exists.
    unsafe { std::slice::from_raw_parts(start, len) }
}

/// Classes by name: every linked static shell, loaded or not, and every
/// class registered with `objc_registerClassPair`.
static REGISTRY: LazyLock<RwLock<HashMap<&'static CStr, Shared<Class>>>> = LazyLock::new(|| {
    let mut classes = HashMap::new();
    for entry in linked() {
        // Two shells with one name can only come from two copies of a
        // framework; the first wins.
        classes.entry(entry.0.name()).or_insert(Shared(entry.0 as *const Class));
    }
    RwLock::new(classes)
});
/// Static shells whose loader is running, waiting to be claimed by
/// `objc_allocateClassPair`.
static PENDING: LazyLock<Mutex<HashMap<&'static CStr, Shared<Class>>>> = LazyLock::new(Default::default);
/// A method table changed: every cached implementation may be stale.
pub(crate) fn bump_epoch() {
    crate::cache::flush_all();
}

/// Empty one class's method cache. For a class still being built, which has
/// no subclasses yet, this is all a method table change needs.
pub(crate) fn reset_cache(cls: &Class) {
    crate::cache::flush(cls);
}

/// Give `cls` a flag saying it overrides a method, and pass it on to the
/// subclasses it already has. Classes registered later inherit it in
/// `objc_registerClassPair`.
pub(crate) fn set_override_flag(cls: &'static Class, flag: u32) {
    debug_assert!(flag & !INHERITED == 0);
    // Under the load lock, like registration: a subclass registered
    // meanwhile either finds the flag on `cls` or is visited here.
    with_load_lock(|| {
        if cls.flags.fetch_or(flag, Ordering::AcqRel) & flag != 0 || !cls.is_loaded() {
            return;
        }
        // Rare: a method added to a class in use. Unloaded shells have no
        // superclass yet, so they are skipped and inherit the flag when
        // they are registered.
        for sub in loaded_classes() {
            if sub.is_subclass_of(cls) {
                sub.flags.fetch_or(flag, Ordering::AcqRel);
            }
        }
    });
}

/// Work out every loaded class's inherited flags afresh, from the methods
/// each class in its superclass chain defines (see
/// `method::own_overrides`). For a change that can take an override away
/// (an implementation set back to the root class's own), which
/// `set_override_flag` can't express.
pub(crate) fn recompute_override_flags() {
    with_load_lock(|| {
        for cls in loaded_classes() {
            let mut flags = 0;
            let mut chain = Some(cls);
            while let Some(c) = chain {
                flags |= crate::method::own_overrides(c);
                chain = c.superclass();
            }
            set_inherited(cls, flags);
        }
    });
}

/// Replace the inherited flags of `cls` with `flags`.
fn set_inherited(cls: &Class, flags: u32) {
    let old = cls.flags();
    if old & INHERITED != flags {
        cls.flags.fetch_or(flags, Ordering::AcqRel);
        cls.flags.fetch_and(!(INHERITED & !flags), Ordering::AcqRel);
    }
}

/// Every loaded class (not metaclass), for the rare changes that must
/// visit them all.
pub(crate) fn loaded_classes() -> Vec<&'static Class> {
    let registry = REGISTRY.read().unwrap();
    // SAFETY: registered classes are never freed.
    registry.values().map(|c| unsafe { c.get() }).filter(|c| c.is_loaded()).collect()
}

/// The class named `name`, which may be a static shell not loaded yet.
pub(crate) fn lookup_name(name: &CStr) -> Option<&'static Class> {
    let registry = REGISTRY.read().unwrap();
    // SAFETY: registered classes are never freed.
    registry.get(name).map(|c| unsafe { c.get() })
}

/// Make sure `cls` (a class or metaclass) has been defined, running its
/// static shell's loader if needed.
pub(crate) fn ensure_loaded(cls: &'static Class) {
    if cls.flags.load(Ordering::Acquire) & LOADED == 0 {
        load_slow(cls);
    }
}

#[cold]
fn load_slow(cls: &'static Class) {
    let target = cls.instance_class();
    with_load_lock(|| {
        let flags = target.flags();
        if flags & LOADED != 0 {
            return;
        }
        if flags & CLAIMED != 0 {
            // Under construction. Loaders run holding this lock, so for a
            // shell this is the defining thread itself (ClassBuilder asking
            // for the superclass, say); a class from objc_allocateClassPair
            // is the caller's to finish before sharing.
            return;
        }
        let name = target.name();
        let Some(loader) = target.loader else {
            panic!("sidestep: class {name:?} was used before objc_registerClassPair");
        };
        lock(&PENDING).insert(name, Shared(target));
        loader();
        let unclaimed = lock(&PENDING).remove(name).is_some();
        if unclaimed || !target.is_loaded() {
            panic!(
                "sidestep: the loader for class {name:?} did not define it. It must register a \
                 class named {name:?} (e.g. by calling a define_class! type's class()), and \
                 nothing may define that class before the runtime asks for it"
            );
        }
    });
}

/// Send `+initialize` to `cls` (not a metaclass) and its superclasses, once.
pub(crate) fn ensure_initialized(cls: &'static Class) {
    if cls.flags.load(Ordering::Acquire) & INITIALIZED == 0 {
        initialize_slow(cls);
    }
}

#[cold]
fn initialize_slow(cls: &'static Class) {
    ensure_loaded(cls);
    with_load_lock(|| {
        if cls.flags() & (INITIALIZED | INITIALIZING) != 0 {
            // Done, or in progress on this thread: messages made by
            // +initialize itself go through.
            return;
        }
        if let Some(superclass) = cls.superclass() {
            ensure_initialized(superclass);
        }
        cls.flags.fetch_or(INITIALIZING, Ordering::AcqRel);
        let sel = known().initialize;
        if let Some(imp) = lookup_imp(cls.metaclass(), sel) {
            // SAFETY: +initialize takes no arguments and returns nothing.
            let imp: unsafe extern "C-unwind" fn(*const Class, Sel) = unsafe { transmute(imp) };
            // SAFETY: `cls` is a loaded class object.
            unsafe { imp(cls, sel) };
        }
        cls.metaclass().flags.fetch_or(INITIALIZED, Ordering::AcqRel);
        cls.flags.fetch_or(INITIALIZED, Ordering::AcqRel);
    });
}

/// Find the method for `sel` on `cls` or its superclasses.
pub(crate) fn find_method(cls: &'static Class, sel: Sel) -> Option<&'static Method> {
    let mut cls = Some(cls);
    while let Some(c) = cls {
        if let Some(m) = c.rt().methods.read().unwrap().by_sel.get(&(sel as usize)) {
            // SAFETY: methods are never freed.
            return Some(unsafe { m.get() });
        }
        cls = c.superclass();
    }
    None
}

/// The implementation of the method `sel` names on `cls`, through the
/// method cache. A cached forwarding trampoline is not a method: classes
/// don't respond to the selectors they forward.
#[inline]
pub(crate) fn lookup_imp(cls: &'static Class, sel: Sel) -> Option<Imp> {
    match crate::cache::probe(cls, sel) {
        Some(imp) if !crate::forward::is_trampoline(imp) => Some(imp),
        _ => lookup_slow(cls, sel),
    }
}

#[cold]
#[inline(never)]
fn lookup_slow(cls: &'static Class, sel: Sel) -> Option<Imp> {
    let epoch = crate::cache::epoch();
    let imp = find_method(cls, sel)?.imp();
    crate::cache::remember(cls, sel, imp, epoch);
    Some(imp)
}

/// Wire up a class and metaclass under `superclass` (or as a new root).
fn init_pair(cls: &'static Class, meta: &'static Class, superclass: Option<&'static Class>) {
    let as_mut = |c: &'static Class| (c as *const Class).cast_mut();
    match superclass {
        Some(sup) => {
            let sup_meta = sup.metaclass();
            cls.superclass.store(as_mut(sup), Ordering::Release);
            meta.superclass.store(as_mut(sup_meta), Ordering::Release);
            // Every metaclass's isa is the root metaclass.
            meta.isa.store(as_mut(sup_meta.metaclass()), Ordering::Release);
            cls.instance_size.store(sup.instance_size(), Ordering::Relaxed);
            cls.instance_align.store(sup.instance_align(), Ordering::Relaxed);
            cls.flags.fetch_or(sup.flags() & INHERITED, Ordering::AcqRel);
        }
        None => {
            cls.superclass.store(std::ptr::null_mut(), Ordering::Release);
            meta.superclass.store(as_mut(cls), Ordering::Release);
            meta.isa.store(as_mut(meta), Ordering::Release);
            cls.instance_size.store(size_of::<Object>(), Ordering::Relaxed);
            cls.instance_align.store(align_of::<Object>(), Ordering::Relaxed);
            cls.flags.fetch_or(ROOT, Ordering::AcqRel);
            meta.flags.fetch_or(ROOT, Ordering::AcqRel);
        }
    }
    cls.isa.store(as_mut(meta), Ordering::Release);
    cls.peer.store(as_mut(meta), Ordering::Release);
    meta.peer.store(as_mut(cls), Ordering::Release);
    cls.flags.fetch_or(CLAIMED, Ordering::AcqRel);
    meta.flags.fetch_or(CLAIMED, Ordering::AcqRel);
}

/// A class pointer from C, loaded, or `None` for null. Classes under
/// construction come back as they are, so class-building functions can use
/// this too; everything that reads or changes a class must, or a static shell
/// seen before its first message would look empty.
pub(crate) unsafe fn class_ref(cls: *const Class) -> Option<&'static Class> {
    // SAFETY: the caller passes a class or null.
    let cls = unsafe { cls.as_ref() }?;
    ensure_loaded(cls);
    Some(cls)
}

/// A class pointer from C, without loading it. Only for functions that read
/// fixed fields (name, meta flag) or finish construction.
pub(crate) unsafe fn class_ref_unloaded(cls: *const Class) -> Option<&'static Class> {
    // SAFETY: the caller passes a class or null.
    unsafe { cls.as_ref() }
}

fn as_ptr(cls: Option<&'static Class>) -> *const Class {
    cls.map_or(std::ptr::null(), |c| c as *const Class)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_allocateClassPair(
    superclass: *const Class,
    name: *const c_char,
    _extra_bytes: usize,
) -> *mut Class {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller passes a C string.
    let name = unsafe { CStr::from_ptr(name) };
    // SAFETY: the caller passes a class or null.
    let superclass = unsafe { superclass.as_ref() };
    with_load_lock(|| {
        if let Some(sup) = superclass {
            ensure_loaded(sup);
        }
        let pending = lock(&PENDING).remove(name);
        if let Some(shell) = pending {
            // SAFETY: shells are statics.
            let shell = unsafe { shell.get() };
            init_pair(shell, shell.peer(), superclass);
            return (shell as *const Class).cast_mut();
        }
        if REGISTRY.read().unwrap().contains_key(name) {
            return std::ptr::null_mut();
        }
        let name = leak_cstr(name);
        let meta: &'static Class = Box::leak(Box::new(Class::dynamic(name, META)));
        let cls: &'static Class = Box::leak(Box::new(Class::dynamic(name, 0)));
        init_pair(cls, meta, superclass);
        (cls as *const Class).cast_mut()
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_registerClassPair(cls: *mut Class) {
    // SAFETY: the caller passes a class from objc_allocateClassPair.
    let Some(cls) = (unsafe { class_ref_unloaded(cls) }) else { return };
    if cls.is_meta() {
        return;
    }
    with_load_lock(|| {
        if let Some((total, offset)) = crate::object::instance_layout(cls, 0) {
            cls.alloc_layout.store(((total as u64) << 32) | offset as u64, Ordering::Relaxed);
        }
        // The flags copied from the superclass when the class was allocated
        // may be out of date: an override added to (or taken from) a
        // superclass since reached only loaded classes. The load lock
        // orders this against those changes.
        let inherited = cls.superclass().map_or(0, |sup| sup.flags() & INHERITED);
        set_inherited(cls, inherited | crate::method::own_overrides(cls));
        cls.metaclass().flags.fetch_or(LOADED, Ordering::AcqRel);
        cls.flags.fetch_or(LOADED, Ordering::AcqRel);
        REGISTRY.write().unwrap().insert(cls.name(), Shared(cls));
        // Registering changes no existing class's methods; only the new
        // class's own caches, if anything messaged it early, start over.
        reset_cache(cls);
        reset_cache(cls.metaclass());
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_disposeClassPair(cls: *mut Class) {
    // SAFETY: the caller passes a class or null.
    let Some(cls) = (unsafe { class_ref_unloaded(cls) }) else { return };
    let mut registry = REGISTRY.write().unwrap();
    if registry.get(cls.name()).is_some_and(|c| std::ptr::eq(c.0, cls)) {
        registry.remove(cls.name());
    }
    // The memory is kept: stale pointers to a disposed class stay harmless.
}

/// Finds static shells too, and loads them, so the class is ready to use
/// however the caller goes on to use it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_getClass(name: *const c_char) -> *const Class {
    if name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes a C string.
    let cls = lookup_name(unsafe { CStr::from_ptr(name) });
    if let Some(cls) = cls {
        ensure_loaded(cls);
    }
    as_ptr(cls)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_lookUpClass(name: *const c_char) -> *const Class {
    // SAFETY: same contract.
    unsafe { objc_getClass(name) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_getRequiredClass(name: *const c_char) -> *const Class {
    // SAFETY: same contract.
    let cls = unsafe { objc_getClass(name) };
    if cls.is_null() {
        // SAFETY: as above.
        let name = unsafe { cstr_or(name, c"(null)") };
        panic!("sidestep: required class {name:?} is not registered");
    }
    cls
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_getMetaClass(name: *const c_char) -> *const Class {
    // SAFETY: same contract.
    let cls = unsafe { objc_getClass(name) };
    // SAFETY: registered classes have a metaclass.
    unsafe { cls.as_ref() }.map_or(std::ptr::null(), |c| c.metaclass() as *const Class)
}

/// Every class, including static shells not loaded yet: they come back as
/// they are, and load when something reads more than their name.
fn registered() -> Vec<*const Class> {
    REGISTRY.read().unwrap().values().map(|c| c.0).collect()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_copyClassList(out_len: *mut u32) -> *mut *const Class {
    // SAFETY: the caller passes a valid or null pointer.
    unsafe { malloc_array(&registered(), out_len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_getClassList(buffer: *mut *const Class, buffer_len: c_int) -> c_int {
    let classes = registered();
    if !buffer.is_null() {
        let n = classes.len().min(buffer_len.max(0) as usize);
        // SAFETY: the caller passes room for `buffer_len` entries.
        unsafe { buffer.copy_from_nonoverlapping(classes.as_ptr(), n) };
    }
    classes.len() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_getName(cls: *const Class) -> *const c_char {
    // SAFETY: the caller passes a class or null.
    match unsafe { class_ref_unloaded(cls) } {
        Some(cls) => cls.name,
        None => c"nil".as_ptr(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_getSuperclass(cls: *const Class) -> *const Class {
    // SAFETY: the caller passes a class or null.
    as_ptr(unsafe { class_ref(cls) }.and_then(Class::superclass))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_isMetaClass(cls: *const Class) -> Bool {
    // SAFETY: the caller passes a class or null.
    match unsafe { class_ref_unloaded(cls) } {
        Some(cls) if cls.is_meta() => YES,
        _ => NO,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_getInstanceSize(cls: *const Class) -> usize {
    // SAFETY: the caller passes a class or null.
    unsafe { class_ref(cls) }.map_or(0, Class::instance_size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_getVersion(cls: *const Class) -> c_int {
    // SAFETY: the caller passes a class or null.
    unsafe { class_ref(cls) }.map_or(0, |c| c.rt().version.load(Ordering::Relaxed))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_setVersion(cls: *mut Class, version: c_int) {
    // SAFETY: the caller passes a class or null.
    if let Some(cls) = unsafe { class_ref(cls) } {
        cls.rt().version.store(version, Ordering::Relaxed);
    }
}

/// Instance variable layouts only mattered to the garbage-collected runtime.
#[unsafe(no_mangle)]
pub extern "C" fn class_getIvarLayout(_cls: *const Class) -> *const u8 {
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub extern "C" fn class_setIvarLayout(_cls: *mut Class, _layout: *const u8) {}
