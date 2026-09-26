//! The runtime's C functions beyond messaging: declared properties,
//! associated objects under each policy, `@synchronized`, the enumeration
//! mutation hook, and class, method and instance variable introspection.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_uint, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::ffi::{self, objc_property_attribute_t as Attribute};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, ClassBuilder, Imp, NSObject, NSZone, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send, sel};

use sidestep as _;

fn attributes(list: &[(&CStr, &CStr)]) -> Vec<Attribute> {
    list.iter().map(|(name, value)| Attribute { name: name.as_ptr(), value: value.as_ptr() }).collect()
}

fn text(ptr: *const c_char) -> Option<String> {
    // SAFETY: the runtime returns C strings or null.
    (!ptr.is_null()).then(|| unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
}

/// A fresh class under `superclass`, for tests that change classes.
fn new_class(name: &CStr, superclass: &AnyClass) -> *mut AnyClass {
    let class = ClassBuilder::new(name, superclass).expect("a new class name").register();
    (class as *const AnyClass).cast_mut()
}

/// Copies a `malloc`ed list the runtime returned, and frees it.
fn take<T: Copy>(list: *mut T, len: c_uint) -> Vec<T> {
    if list.is_null() {
        return Vec::new();
    }
    // SAFETY: the runtime returned `len` entries.
    let items = unsafe { std::slice::from_raw_parts(list, len as usize) }.to_vec();
    // SAFETY: allocated with malloc.
    unsafe { ffi::free(list.cast()) };
    items
}

#[test]
fn class_properties() {
    let base = new_class(c"SidestepAbiPropertyBase", NSObject::class());
    let name = attributes(&[(c"T", c"@\"NSString\""), (c"C", c""), (c"N", c""), (c"V", c"_name")]);
    let count = attributes(&[(c"T", c"q"), (c"R", c"")]);
    unsafe {
        assert!(ffi::class_addProperty(base, c"name".as_ptr(), name.as_ptr(), 4).as_bool());
        assert!(!ffi::class_addProperty(base, c"name".as_ptr(), count.as_ptr(), 2).as_bool());
        assert!(ffi::class_addProperty(base, c"count".as_ptr(), count.as_ptr(), 2).as_bool());

        let property = ffi::class_getProperty(base, c"name".as_ptr());
        assert!(!property.is_null());
        assert_eq!(text(ffi::property_getName(property)).as_deref(), Some("name"));
        assert_eq!(text(ffi::property_getAttributes(property)).as_deref(), Some("T@\"NSString\",C,N,V_name"));
        // The strings may live in the list's own allocation, so they are
        // read before it is freed.
        let mut len = 0;
        let raw = ffi::property_copyAttributeList(property, &mut len);
        let list: Vec<(String, String)> = std::slice::from_raw_parts(raw, len as usize)
            .iter()
            .map(|a| (text(a.name).unwrap(), text(a.value).unwrap()))
            .collect();
        ffi::free(raw.cast());
        assert_eq!(
            list,
            [("T", "@\"NSString\""), ("C", ""), ("N", ""), ("V", "_name")].map(|(n, v)| (n.into(), v.into()))
        );
        let value = ffi::property_copyAttributeValue(property, c"V".as_ptr());
        assert_eq!(text(value).as_deref(), Some("_name"));
        ffi::free(value.cast());
        let flag = ffi::property_copyAttributeValue(property, c"N".as_ptr());
        assert_eq!(text(flag).as_deref(), Some(""));
        ffi::free(flag.cast());
        assert!(ffi::property_copyAttributeValue(property, c"W".as_ptr()).is_null());

        // A class lists its own properties, newest first; lookups also
        // search superclasses; class properties are the metaclass's.
        let sub = new_class(c"SidestepAbiPropertySub", &*base);
        assert_eq!(ffi::class_getProperty(sub, c"name".as_ptr()), property);
        let mut len = 0;
        assert!(take(ffi::class_copyPropertyList(sub, &mut len), len).is_empty());
        assert_eq!(property_names(base), ["count", "name"]);
        assert!(ffi::class_getProperty((*base).metaclass(), c"name".as_ptr()).is_null());
        assert!(ffi::class_getProperty(base, c"missing".as_ptr()).is_null());

        // Replacing changes a property in place, or adds it.
        let count = ffi::class_getProperty(base, c"count".as_ptr());
        let double = attributes(&[(c"T", c"d")]);
        ffi::class_replaceProperty(base, c"count".as_ptr(), double.as_ptr(), 1);
        assert_eq!(ffi::class_getProperty(base, c"count".as_ptr()), count);
        assert_eq!(text(ffi::property_getAttributes(count)).as_deref(), Some("Td"));
        ffi::class_replaceProperty(base, c"fresh".as_ptr(), double.as_ptr(), 1);
        assert!(!ffi::class_getProperty(base, c"fresh".as_ptr()).is_null());
        assert_eq!(property_names(base), ["fresh", "count", "name"]);
    }
}

/// The names of a class's own properties, in the runtime's order.
fn property_names(cls: *mut AnyClass) -> Vec<String> {
    let mut len = 0;
    // SAFETY: a class; the list is freed by `take`.
    let list = take(unsafe { ffi::class_copyPropertyList(cls, &mut len) }, len);
    // SAFETY: properties have names.
    list.iter().map(|&p| text(unsafe { ffi::property_getName(p) }).unwrap()).collect()
}

/// The attribute string is the property's description: an attribute given
/// without a value is left out, and a name longer than one character is
/// quoted.
#[test]
fn property_attribute_strings() {
    let cls = new_class(c"SidestepAbiPropertyStrings", NSObject::class());
    let given = [
        Attribute { name: c"T".as_ptr(), value: c"@".as_ptr() },
        Attribute { name: c"Custom".as_ptr(), value: c"x".as_ptr() },
        Attribute { name: c"N".as_ptr(), value: std::ptr::null() },
        Attribute { name: c"&".as_ptr(), value: c"".as_ptr() },
    ];
    unsafe {
        assert!(ffi::class_addProperty(cls, c"p".as_ptr(), given.as_ptr(), given.len() as c_uint).as_bool());
        let property = ffi::class_getProperty(cls, c"p".as_ptr());
        assert_eq!(text(ffi::property_getAttributes(property)).as_deref(), Some("T@,\"Custom\"x,&"));
        let mut len = 0;
        let raw = ffi::property_copyAttributeList(property, &mut len);
        let list: Vec<(String, String)> = std::slice::from_raw_parts(raw, len as usize)
            .iter()
            .map(|a| (text(a.name).unwrap(), text(a.value).unwrap()))
            .collect();
        ffi::free(raw.cast());
        assert_eq!(list, [("T", "@"), ("Custom", "x"), ("&", "")].map(|(n, v)| (n.into(), v.into())));
        let custom = ffi::property_copyAttributeValue(property, c"Custom".as_ptr());
        assert_eq!(text(custom).as_deref(), Some("x"));
        ffi::free(custom.cast());
        assert!(ffi::property_copyAttributeValue(property, c"N".as_ptr()).is_null());

        // No attributes at all.
        assert!(ffi::class_addProperty(cls, c"bare".as_ptr(), std::ptr::null(), 0).as_bool());
        let bare = ffi::class_getProperty(cls, c"bare".as_ptr());
        assert_eq!(text(ffi::property_getAttributes(bare)).as_deref(), Some(""));
        let mut len = 1;
        assert!(ffi::property_copyAttributeList(bare, &mut len).is_null());
        assert_eq!(len, 0);
    }
}

#[test]
fn protocol_properties() {
    let name = attributes(&[(c"T", c"@"), (c"&", c""), (c"N", c"")]);
    let flag = attributes(&[(c"T", c"B"), (c"R", c"")]);
    unsafe {
        let proto = ffi::objc_allocateProtocol(c"SidestepAbiPropertyProtocol".as_ptr());
        assert!(!proto.is_null());
        ffi::protocol_addProperty(proto, c"title".as_ptr(), name.as_ptr(), 3, true.into(), true.into());
        ffi::protocol_addProperty(proto, c"shared".as_ptr(), flag.as_ptr(), 2, true.into(), false.into());
        ffi::objc_registerProtocol(proto);

        let title = ffi::protocol_getProperty(proto, c"title".as_ptr(), true.into(), true.into());
        assert_eq!(text(ffi::property_getAttributes(title)).as_deref(), Some("T@,&,N"));
        assert!(ffi::protocol_getProperty(proto, c"title".as_ptr(), true.into(), false.into()).is_null());
        let shared = ffi::protocol_getProperty(proto, c"shared".as_ptr(), true.into(), false.into());
        assert_eq!(text(ffi::property_getName(shared)).as_deref(), Some("shared"));
        let mut len = 0;
        let listed = take(ffi::protocol_copyPropertyList(proto, &mut len), len);
        assert_eq!(listed, [title]);

        // A protocol finds the properties of the protocols it adopts.
        let outer = ffi::objc_allocateProtocol(c"SidestepAbiPropertyOuter".as_ptr());
        ffi::protocol_addProtocol(outer, proto);
        ffi::objc_registerProtocol(outer);
        assert_eq!(ffi::protocol_getProperty(outer, c"title".as_ptr(), true.into(), true.into()), title);
        assert!(take(ffi::protocol_copyPropertyList(outer, &mut len), len).is_empty());
        assert!(AnyProtocol::get(c"SidestepAbiPropertyOuter").is_some());
    }
}

/// Counts its deallocations and copies.
struct ValueIvars {
    drops: Arc<AtomicUsize>,
    copies: Arc<AtomicUsize>,
}

impl Drop for ValueIvars {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepAbiValue"]
    #[ivars = ValueIvars]
    struct Value;

    impl Value {
        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut c_void) -> Retained<Value> {
            self.ivars().copies.fetch_add(1, Ordering::SeqCst);
            Value::new(&self.ivars().drops, &self.ivars().copies)
        }
    }
);

impl Value {
    fn new(drops: &Arc<AtomicUsize>, copies: &Arc<AtomicUsize>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ValueIvars { drops: drops.clone(), copies: copies.clone() });
        unsafe { msg_send![super(this), init] }
    }
}

static KEY_A: u8 = 0;
static KEY_B: u8 = 0;

fn key(k: &'static u8) -> *const c_void {
    (k as *const u8).cast()
}

fn set(owner: &AnyObject, k: &'static u8, value: Option<&AnyObject>, policy: ffi::objc_AssociationPolicy) {
    let owner = (owner as *const AnyObject).cast_mut();
    let value = value.map_or(std::ptr::null_mut(), |v| (v as *const AnyObject).cast_mut());
    unsafe { ffi::objc_setAssociatedObject(owner, key(k), value, policy) };
}

fn get(owner: &AnyObject, k: &'static u8) -> *const AnyObject {
    unsafe { ffi::objc_getAssociatedObject(owner, key(k)) }
}

#[test]
fn associated_objects_by_policy() {
    let (drops, copies) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let owner = NSObject::new();
    autoreleasepool(|_| {
        // Retained: the association keeps the value alive, and releases it
        // when replaced.
        let value = Value::new(&drops, &copies);
        set(&owner, &KEY_A, Some(&value), ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        assert_eq!(get(&owner, &KEY_A), Retained::as_ptr(&value).cast());
        drop(value);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        set(&owner, &KEY_A, None, ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(get(&owner, &KEY_A).is_null());

        // Assigned: not retained.
        let value = Value::new(&drops, &copies);
        set(&owner, &KEY_A, Some(&value), ffi::OBJC_ASSOCIATION_ASSIGN);
        assert_eq!(get(&owner, &KEY_A), Retained::as_ptr(&value).cast());
        set(&owner, &KEY_A, None, ffi::OBJC_ASSOCIATION_ASSIGN);
        drop(value);
        assert_eq!(drops.load(Ordering::SeqCst), 2);

        // Copied: the association holds a copy.
        let value = Value::new(&drops, &copies);
        set(&owner, &KEY_A, Some(&value), ffi::OBJC_ASSOCIATION_COPY_NONATOMIC);
        assert_eq!(copies.load(Ordering::SeqCst), 1);
        assert_ne!(get(&owner, &KEY_A), Retained::as_ptr(&value).cast());
        drop(value);
        assert_eq!(drops.load(Ordering::SeqCst), 3);
        set(&owner, &KEY_A, None, ffi::OBJC_ASSOCIATION_COPY);
        assert_eq!(drops.load(Ordering::SeqCst), 4);
    });
}

#[test]
fn atomic_getter_keeps_the_value_alive() {
    let (drops, copies) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let owner = NSObject::new();
    autoreleasepool(|_| {
        set(&owner, &KEY_A, Some(&Value::new(&drops, &copies)), ffi::OBJC_ASSOCIATION_RETAIN);
        let got = get(&owner, &KEY_A);
        // Replacing the value releases the association's reference, but
        // the getter handed out one of its own, autoreleased.
        set(&owner, &KEY_A, None, ffi::OBJC_ASSOCIATION_RETAIN);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let alive: bool = unsafe { msg_send![&*got, isKindOfClass: Value::class()] };
        assert!(alive);
    });
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn associated_objects_released_with_their_owner() {
    let (drops, copies) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let owner = NSObject::new();
    set(&owner, &KEY_A, Some(&Value::new(&drops, &copies)), ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    set(&owner, &KEY_B, Some(&Value::new(&drops, &copies)), ffi::OBJC_ASSOCIATION_RETAIN);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(owner);
    assert_eq!(drops.load(Ordering::SeqCst), 2);

    let owner = NSObject::new();
    set(&owner, &KEY_A, Some(&Value::new(&drops, &copies)), ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    set(&owner, &KEY_B, Some(&Value::new(&drops, &copies)), ffi::OBJC_ASSOCIATION_COPY_NONATOMIC);
    unsafe { ffi::objc_removeAssociatedObjects(Retained::as_ptr(&owner).cast_mut().cast()) };
    assert_eq!(drops.load(Ordering::SeqCst), 5);
    assert!(get(&owner, &KEY_A).is_null() && get(&owner, &KEY_B).is_null());
}

#[test]
fn associated_objects_across_threads() {
    let drops = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let drops = drops.clone();
            std::thread::spawn(move || {
                let copies = Arc::new(AtomicUsize::new(0));
                for _ in 0..500 {
                    let owner = NSObject::new();
                    set(&owner, &KEY_A, Some(&Value::new(&drops, &copies)), ffi::OBJC_ASSOCIATION_RETAIN);
                    autoreleasepool(|_| assert!(!get(&owner, &KEY_A).is_null()));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(drops.load(Ordering::SeqCst), 2000);
}

#[test]
fn synchronized() {
    let obj = NSObject::new();
    let raw = Retained::as_ptr(&obj).cast_mut().cast::<AnyObject>();
    unsafe {
        // Exiting a lock not held fails; nil is a no-op.
        assert_eq!(ffi::objc_sync_exit(raw), -1);
        assert_eq!(ffi::objc_sync_enter(std::ptr::null_mut()), 0);
        assert_eq!(ffi::objc_sync_exit(std::ptr::null_mut()), 0);
        // Recursive.
        assert_eq!(ffi::objc_sync_enter(raw), 0);
        assert_eq!(ffi::objc_sync_enter(raw), 0);
        assert_eq!(ffi::objc_sync_exit(raw), 0);
        assert_eq!(ffi::objc_sync_exit(raw), 0);
        assert_eq!(ffi::objc_sync_exit(raw), -1);
    }

    // Mutual exclusion: a read-modify-write under the lock never loses an
    // update.
    struct Shared {
        obj: Retained<NSObject>,
        count: Cell<usize>,
    }
    unsafe impl Send for Shared {}
    unsafe impl Sync for Shared {}
    let shared = Arc::new(Shared { obj, count: Cell::new(0) });
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let shared = shared.clone();
            std::thread::spawn(move || {
                let raw = Retained::as_ptr(&shared.obj).cast_mut().cast::<AnyObject>();
                for _ in 0..2000 {
                    unsafe { ffi::objc_sync_enter(raw) };
                    let n = shared.count.get();
                    std::hint::black_box(n);
                    shared.count.set(n + 1);
                    unsafe { ffi::objc_sync_exit(raw) };
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(shared.count.get(), 8000);
}

#[test]
fn synchronized_objects_are_freed() {
    let drops = Arc::new(AtomicUsize::new(0));
    let copies = Arc::new(AtomicUsize::new(0));
    for _ in 0..100 {
        let value = Value::new(&drops, &copies);
        let raw = Retained::as_ptr(&value).cast_mut().cast::<AnyObject>();
        unsafe {
            assert_eq!(ffi::objc_sync_enter(raw), 0);
            assert_eq!(ffi::objc_sync_exit(raw), 0);
        }
    }
    assert_eq!(drops.load(Ordering::SeqCst), 100);
}

unsafe extern "C-unwind" {
    fn objc_enumerationMutation(obj: *mut AnyObject);
}
unsafe extern "C" {
    fn objc_setEnumerationMutationHandler(handler: Option<unsafe extern "C-unwind" fn(*mut AnyObject)>);
}

static MUTATED: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C-unwind" fn on_mutation(obj: *mut AnyObject) {
    MUTATED.store(obj as usize, Ordering::SeqCst);
}

#[test]
fn enumeration_mutation_handler() {
    let collection = NSObject::new();
    let raw = Retained::as_ptr(&collection).cast_mut().cast::<AnyObject>();
    unsafe {
        objc_setEnumerationMutationHandler(Some(on_mutation));
        objc_enumerationMutation(raw);
    }
    assert_eq!(MUTATED.load(Ordering::SeqCst), raw as usize);
}

extern "C-unwind" fn twice(_: &AnyObject, _: Sel, x: i64) -> i64 {
    x * 2
}

extern "C-unwind" fn class_twice(_: &AnyClass, _: Sel, x: i64) -> i64 {
    x * 2
}

#[test]
fn methods_and_ivars_by_hand() {
    let mut builder = ClassBuilder::new(c"SidestepAbiHandmade", NSObject::class()).unwrap();
    builder.add_ivar::<u8>(c"flag");
    builder.add_ivar::<i64>(c"count");
    builder.add_ivar::<*mut AnyObject>(c"child");
    unsafe {
        builder.add_method(sel!(twice:), twice as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_class_method(sel!(classTwice:), class_twice as extern "C-unwind" fn(_, _, _) -> _);
    }
    let class = builder.register();

    let ivars = class.instance_variables();
    let names: Vec<_> = ivars.iter().map(|i| i.name().to_owned()).collect();
    assert_eq!(names, [c"flag", c"count", c"child"].map(CStr::to_owned));
    let count = class.instance_variable(c"count").unwrap();
    assert_eq!(count.type_encoding(), c"q");
    assert_eq!(count.offset() % 8, 0);
    assert!(count.offset() >= NSObject::class().instance_size() as isize);
    assert!(class.instance_size() >= count.offset() as usize + 8);
    assert!(class.instance_variable(c"missing").is_none());
    assert!(
        NSObject::class().instance_variables().is_empty() || NSObject::class().instance_variable(c"count").is_none()
    );

    let instance_methods: Vec<Sel> = class.instance_methods().iter().map(|m| m.name()).collect();
    assert_eq!(instance_methods, [sel!(twice:)]);
    let class_methods: Vec<Sel> = class.metaclass().instance_methods().iter().map(|m| m.name()).collect();
    assert_eq!(class_methods, [sel!(classTwice:)]);
    let method = class.instance_method(sel!(twice:)).unwrap();
    assert_eq!(method.arguments_count(), 3);
    assert_eq!(method.return_type().to_str().unwrap(), "q");
    assert_eq!(method.argument_type(2).unwrap().to_str().unwrap(), "q");
    assert!(method.argument_type(3).is_none());

    let obj: Retained<AnyObject> = unsafe { msg_send![class, new] };
    unsafe {
        *count.load_mut::<i64>(&mut *(Retained::as_ptr(&obj).cast_mut())) = 21;
        assert_eq!(*count.load::<i64>(&obj), 21);
        let doubled: i64 = msg_send![&*obj, twice: 21i64];
        assert_eq!(doubled, 42);
        let doubled: i64 = msg_send![class, classTwice: 4i64];
        assert_eq!(doubled, 8);
    }
}

#[test]
fn nil_classes() {
    unsafe {
        let mut len = 7;
        assert!(ffi::class_copyMethodList(std::ptr::null(), &mut len).is_null());
        assert_eq!(len, 0);
        assert_eq!(text(ffi::class_getName(std::ptr::null())).as_deref(), Some("nil"));
        assert_eq!(text(ffi::object_getClassName(std::ptr::null())).as_deref(), Some("nil"));
        assert!(ffi::class_getSuperclass(std::ptr::null()).is_null());
        assert!(!ffi::class_isMetaClass(std::ptr::null()).as_bool());
        assert_eq!(ffi::class_getInstanceSize(std::ptr::null()), 0);
    }
}

static ZONE_ALLOCS: AtomicUsize = AtomicUsize::new(0);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepAbiZoneAllocated"]
    struct ZoneAllocated;

    impl ZoneAllocated {
        #[unsafe(method(allocWithZone:))]
        fn alloc_with_zone(zone: *mut NSZone) -> *mut ZoneAllocated {
            ZONE_ALLOCS.fetch_add(1, Ordering::SeqCst);
            unsafe { msg_send![super(Self::class(), NSObject::class().metaclass()), allocWithZone: zone] }
        }
    }
);

#[test]
fn alloc_goes_through_alloc_with_zone() {
    let before = ZONE_ALLOCS.load(Ordering::SeqCst);
    let a: Retained<ZoneAllocated> = unsafe { msg_send![ZoneAllocated::alloc(), init] };
    let b: Retained<ZoneAllocated> = unsafe { msg_send![ZoneAllocated::class(), new] };
    assert_eq!(ZONE_ALLOCS.load(Ordering::SeqCst) - before, 2);
    assert!(a.class() == ZoneAllocated::class() && b.class() == ZoneAllocated::class());
}

/// A heap block is an object: freeing it releases what is associated with
/// it and zeroes weak references to it.
#[test]
fn heap_blocks_leave_the_side_tables_when_freed() {
    let (drops, copies) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let block = block2::RcBlock::new(|| {});
    let ptr = block2::RcBlock::as_ptr(&block).cast::<AnyObject>();
    // SAFETY: a heap block is an object, alive while `block` is.
    set(unsafe { &*ptr }, &KEY_A, Some(&Value::new(&drops, &copies)), ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    let mut weak: *mut AnyObject = std::ptr::null_mut();
    unsafe {
        ffi::objc_initWeak(&mut weak, ptr);
        let loaded = ffi::objc_loadWeakRetained(&mut weak);
        assert_eq!(loaded, ptr);
        ffi::objc_release(loaded);
    }
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(block);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    unsafe {
        assert!(ffi::objc_loadWeakRetained(&mut weak).is_null());
        ffi::objc_destroyWeak(&mut weak);
    }
}

static LATE_RETAINS: AtomicUsize = AtomicUsize::new(0);
static LATE_ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C-unwind" fn late_retain(this: *mut AnyObject, sel: Sel) -> *mut AnyObject {
    LATE_RETAINS.fetch_add(1, Ordering::SeqCst);
    let root: unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject =
        unsafe { std::mem::transmute(NSObject::class().instance_method(sel).unwrap().implementation()) };
    unsafe { root(this, sel) }
}

unsafe extern "C-unwind" fn late_alloc_with_zone(cls: *const AnyClass, sel: Sel, zone: *mut NSZone) -> *mut AnyObject {
    LATE_ALLOCS.fetch_add(1, Ordering::SeqCst);
    let root: unsafe extern "C-unwind" fn(*const AnyClass, Sel, *mut NSZone) -> *mut AnyObject =
        unsafe { std::mem::transmute(NSObject::class().class_method(sel).unwrap().implementation()) };
    unsafe { root(cls, sel, zone) }
}

/// A `-retain` or `+allocWithZone:` added to a class in use reaches its
/// subclasses, including one still being built when the method was added.
#[test]
fn overrides_added_later_reach_subclasses() {
    unsafe {
        let base = new_class(c"SidestepAbiLateRetainBase", NSObject::class());
        let building = ClassBuilder::new(c"SidestepAbiLateRetainSub", &*base).unwrap();
        let retain =
            std::mem::transmute::<unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject, Imp>(late_retain);
        assert!(ffi::class_addMethod(base, sel!(retain), retain, c"@@:".as_ptr()).as_bool());
        let sub = building.register();
        let obj: Retained<AnyObject> = msg_send![sub, new];
        let before = LATE_RETAINS.load(Ordering::SeqCst);
        drop(obj.clone());
        assert_eq!(LATE_RETAINS.load(Ordering::SeqCst), before + 1);

        let base = new_class(c"SidestepAbiLateAllocBase", NSObject::class());
        let sub = new_class(c"SidestepAbiLateAllocSub", &*base);
        let meta = ((*base).metaclass() as *const AnyClass).cast_mut();
        type AllocWithZone = unsafe extern "C-unwind" fn(*const AnyClass, Sel, *mut NSZone) -> *mut AnyObject;
        let alloc = std::mem::transmute::<AllocWithZone, Imp>(late_alloc_with_zone);
        let types = c"@@:^{_NSZone=}";
        assert!(ffi::class_addMethod(meta, sel!(allocWithZone:), alloc, types.as_ptr()).as_bool());
        let obj: Retained<AnyObject> = msg_send![sub, new];
        assert_eq!(LATE_ALLOCS.load(Ordering::SeqCst), 1);
        assert!(obj.class() == &*sub);
    }
}
