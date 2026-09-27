//! Toll-free bridging's shared parts: `CFGetTypeID` and each bridged
//! type's type ID, `CFCopyDescription`, and the allocator constants.
//!
//! CoreFoundation objects are Foundation's objects here, so a type ID is
//! worked out from the object's class: the first class on its superclass
//! chain that has a CoreFoundation counterpart. Classes are compared by
//! name, which needs no link-time reference to classes (such as the
//! collections) that may not be built in. The ID values are Sidestep's
//! own; only their equality means anything.
//!
//! Allocators are ignored (everything is allocated the Objective-C way),
//! so the allocator constants are distinct immortal objects that nothing
//! looks inside.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class, msg_send};
use objc2_foundation::NSString;
use sidestep_runtime::{ObjectRef, StaticObject};

pub(crate) type CFTypeID = usize;

/// The type IDs, by kind.
pub(crate) mod id {
    use super::CFTypeID;
    pub(crate) const TYPE: CFTypeID = 1;
    pub(crate) const ALLOCATOR: CFTypeID = 2;
    pub(crate) const STRING: CFTypeID = 7;
    pub(crate) const DICTIONARY: CFTypeID = 18;
    pub(crate) const ARRAY: CFTypeID = 19;
    pub(crate) const DATA: CFTypeID = 20;
    pub(crate) const BOOLEAN: CFTypeID = 21;
    pub(crate) const NUMBER: CFTypeID = 22;
    pub(crate) const NULL: CFTypeID = 23;
    pub(crate) const SET: CFTypeID = 24;
    pub(crate) const URL: CFTypeID = 29;
    pub(crate) const ERROR: CFTypeID = 30;
    pub(crate) const ATTRIBUTED_STRING: CFTypeID = 31;
    pub(crate) const CHARACTER_SET: CFTypeID = 32;
    pub(crate) const DATE: CFTypeID = 42;
    pub(crate) const RUN_LOOP: CFTypeID = 43;
    pub(crate) const RUN_LOOP_TIMER: CFTypeID = 44;
    pub(crate) const RUN_LOOP_OBSERVER: CFTypeID = 45;
    pub(crate) const LOCALE: CFTypeID = 46;
    pub(crate) const TIME_ZONE: CFTypeID = 47;
    pub(crate) const RUN_LOOP_SOURCE: CFTypeID = 48;
}

/// Class names with CoreFoundation counterparts.
const BRIDGED: &[(&str, CFTypeID)] = &[
    ("NSString", id::STRING),
    ("NSAttributedString", id::ATTRIBUTED_STRING),
    ("NSDictionary", id::DICTIONARY),
    ("NSArray", id::ARRAY),
    ("NSSet", id::SET),
    ("NSData", id::DATA),
    ("NSNumber", id::NUMBER),
    ("NSNull", id::NULL),
    ("NSURL", id::URL),
    ("NSError", id::ERROR),
    ("NSDate", id::DATE),
    ("NSCharacterSet", id::CHARACTER_SET),
    ("NSLocale", id::LOCALE),
    ("NSTimeZone", id::TIME_ZONE),
    ("NSTimer", id::RUN_LOOP_TIMER),
    ("_SidestepCFRunLoop", id::RUN_LOOP),
    ("_SidestepRunLoopObserver", id::RUN_LOOP_OBSERVER),
    ("_SidestepCFAllocator", id::ALLOCATOR),
];

/// The type ID of an object's class.
pub(crate) fn type_of(object: &AnyObject) -> CFTypeID {
    let mut class: Option<&AnyClass> = Some(object.class());
    while let Some(c) = class {
        let name = c.name().to_bytes();
        if let Some(&(_, id)) = BRIDGED.iter().find(|(n, _)| n.as_bytes() == name) {
            if id == id::NUMBER && is_boolean(object) {
                return id::BOOLEAN;
            }
            return id;
        }
        class = c.superclass();
    }
    id::TYPE
}

/// A boolean: one of the two constants `+numberWithBool:` hands out
/// (`kCFBooleanTrue` and `kCFBooleanFalse`). A number made from a `char`
/// is a number, as on macOS.
pub(crate) fn is_boolean(number: &AnyObject) -> bool {
    crate::number::is_boolean(number)
}

/// # Safety
///
/// `cf` is a live object.
pub(crate) unsafe fn object<'a>(cf: *const c_void) -> &'a AnyObject {
    // SAFETY: guaranteed by the caller.
    unsafe { &*cf.cast::<AnyObject>() }
}

/// A +1 reference, as the Create and Copy functions return.
pub(crate) fn owned<T: objc2::Message>(object: Retained<T>) -> *mut c_void {
    Retained::into_raw(object).cast()
}

/// Keep `value` alive as long as `owner`, for the Get functions, whose
/// results the caller doesn't own.
pub(crate) fn owned_by(owner: &AnyObject, key: &'static u8, value: Retained<AnyObject>) -> *const c_void {
    let ptr = Retained::as_ptr(&value).cast::<c_void>();
    // SAFETY: retaining associations keep `value` for `owner`'s lifetime;
    // the key is a static address.
    unsafe {
        objc2::ffi::objc_setAssociatedObject(
            (owner as *const AnyObject).cast_mut(),
            (key as *const u8).cast(),
            Retained::as_ptr(&value).cast_mut(),
            objc2::ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        )
    };
    ptr
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFGetTypeID(cf: *const c_void) -> CFTypeID {
    if cf.is_null() {
        return 0;
    }
    // SAFETY: the caller passes a live object.
    type_of(unsafe { object(cf) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCopyDescription(cf: *const c_void) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller passes a live object; -description returns a string.
    let text: Retained<NSString> = unsafe { msg_send![object(cf), description] };
    owned(text)
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCopyTypeIDDescription(type_id: CFTypeID) -> *mut c_void {
    let name = match type_id {
        id::TYPE => "CFType",
        id::ALLOCATOR => "CFAllocator",
        id::BOOLEAN => "CFBoolean",
        id::RUN_LOOP => "CFRunLoop",
        id::RUN_LOOP_TIMER => "CFRunLoopTimer",
        id::RUN_LOOP_OBSERVER => "CFRunLoopObserver",
        _ => BRIDGED.iter().find(|(_, id)| *id == type_id).map_or("", |(n, _)| n.strip_prefix("NS").unwrap_or(n)),
    };
    let name = if name.starts_with("CF") || name.is_empty() { name.to_string() } else { format!("CF{name}") };
    owned(NSString::from_str(&name))
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFShow(cf: *const c_void) {
    if cf.is_null() {
        eprintln!("(null)");
        return;
    }
    // SAFETY: the caller passes a live object.
    let text: Retained<NSString> = unsafe { msg_send![object(cf), description] };
    eprintln!("{text}");
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFGetAllocator(_cf: *const c_void) -> *const c_void {
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFAllocatorGetTypeID() -> CFTypeID {
    id::ALLOCATOR
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFAllocatorGetDefault() -> *const c_void {
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFNullGetTypeID() -> CFTypeID {
    id::NULL
}

sidestep_runtime::static_class!(pub(crate) CF_ALLOCATOR_CLASS, CF_ALLOCATOR_META = "_SidestepCFAllocator", || {
    let _ = AllocatorImpl::class();
});

define_class!(
    /// The class of the allocator constants.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCFAllocator"]
    struct AllocatorImpl;
);

static SYSTEM_DEFAULT: StaticObject<()> = StaticObject::new(&CF_ALLOCATOR_CLASS, ());
static MALLOC: StaticObject<()> = StaticObject::new(&CF_ALLOCATOR_CLASS, ());
static MALLOC_ZONE: StaticObject<()> = StaticObject::new(&CF_ALLOCATOR_CLASS, ());
static NULL_ALLOCATOR: StaticObject<()> = StaticObject::new(&CF_ALLOCATOR_CLASS, ());
static USE_CONTEXT: StaticObject<()> = StaticObject::new(&CF_ALLOCATOR_CLASS, ());

/// `NULL`: the default allocator.
#[unsafe(no_mangle)]
pub static kCFAllocatorDefault: usize = 0;
#[unsafe(no_mangle)]
pub static kCFAllocatorSystemDefault: ObjectRef = SYSTEM_DEFAULT.object_ref();
#[unsafe(no_mangle)]
pub static kCFAllocatorMalloc: ObjectRef = MALLOC.object_ref();
#[unsafe(no_mangle)]
pub static kCFAllocatorMallocZone: ObjectRef = MALLOC_ZONE.object_ref();
#[unsafe(no_mangle)]
pub static kCFAllocatorNull: ObjectRef = NULL_ALLOCATOR.object_ref();
#[unsafe(no_mangle)]
pub static kCFAllocatorUseContext: ObjectRef = USE_CONTEXT.object_ref();

/// Whether an allocator is `kCFAllocatorNull`, which frees nothing.
pub(crate) fn is_null_allocator(allocator: *const c_void) -> bool {
    std::ptr::eq(allocator.cast::<u8>(), NULL_ALLOCATOR.as_object().cast_const().cast::<u8>())
}
