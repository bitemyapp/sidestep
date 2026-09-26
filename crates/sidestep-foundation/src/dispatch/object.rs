//! Dispatch objects. They are Objective-C objects, as on macOS, so
//! `dispatch_retain` is `objc_retain` and dispatch2's `Message` impls and
//! block captures work. One class, `OS_dispatch_object`, serves every kind;
//! its instances keep their state ([`Body`]) right after the `isa`, as
//! extra bytes of a heap object or as the body of a [`StaticObject`] for
//! the immortal ones (the main queue, the global queues, the queue
//! attributes). The class's `-dealloc` drops the state and runs the
//! finalizer with the context.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};

use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, Sel};
use objc2::{ClassType, msg_send, sel};
use sidestep_runtime::StaticObject;

use super::group::Group;
use super::queue::Queue;
use super::semaphore::Semaphore;
use super::source::Source;

/// The C function type libdispatch passes work as.
pub(crate) type Function = unsafe extern "C-unwind" fn(*mut c_void);

pub(crate) enum Kind {
    Queue(Queue),
    /// A queue attribute. Only these two properties change what a queue
    /// does here; quality of service and autorelease frequency don't.
    Attribute {
        concurrent: bool,
        inactive: bool,
    },
    Group(Group),
    Semaphore(Semaphore),
    Source(Source),
}

/// A dispatch object's state.
pub(crate) struct Body {
    pub(crate) kind: Kind,
    context: AtomicPtr<c_void>,
    finalizer: AtomicPtr<()>,
}

// Heap objects keep the body in extra bytes after an 8-byte instance, and
// static objects right after the isa; both are 8 bytes past the object only
// if the body needs no more than pointer alignment.
const _: () = assert!(align_of::<Body>() <= 8);

impl Body {
    pub(crate) const fn new(kind: Kind) -> Body {
        Body { kind, context: AtomicPtr::new(std::ptr::null_mut()), finalizer: AtomicPtr::new(std::ptr::null_mut()) }
    }
}

pub(crate) type Immortal = StaticObject<Body>;

sidestep_runtime::static_class!(pub(crate) DISPATCH_CLASS, DISPATCH_META = "OS_dispatch_object", load);

/// The state of a dispatch object.
///
/// # Safety
/// `object` must be a live dispatch object.
pub(crate) unsafe fn body<'a>(object: *const c_void) -> &'a Body {
    // SAFETY: dispatch objects keep their body 8 bytes past the object.
    unsafe { &*object.cast::<u8>().add(8).cast::<Body>() }
}

/// A new dispatch object, with one reference.
pub(crate) fn create(kind: Kind) -> *mut c_void {
    // Load the class from its shell before making instances of it.
    // SAFETY: the shell is a class; +class loads it.
    let class: *const AnyClass =
        unsafe { msg_send![(&DISPATCH_CLASS as *const sidestep_runtime::Class).cast::<AnyClass>(), class] };
    // SAFETY: a registered class; the extra bytes hold the body.
    let object = unsafe { objc2::ffi::class_createInstance(class, size_of::<Body>()) };
    assert!(!object.is_null(), "sidestep: out of memory for a dispatch object");
    // SAFETY: the extra bytes are ours, suitably aligned (see above), and
    // uninitialized until now.
    unsafe { object.cast::<u8>().add(8).cast::<Body>().write(Body::new(kind)) };
    object.cast()
}

pub(crate) fn context(object: *const c_void) -> *mut c_void {
    // SAFETY: callers pass dispatch objects.
    unsafe { body(object) }.context.load(Ordering::Acquire)
}

pub(crate) fn set_context(object: *const c_void, context: *mut c_void) {
    // SAFETY: as above.
    unsafe { body(object) }.context.store(context, Ordering::Release);
}

pub(crate) fn set_finalizer(object: *const c_void, finalizer: Option<Function>) {
    let pointer = finalizer.map_or(std::ptr::null_mut(), |f| f as *mut ());
    // SAFETY: as above.
    unsafe { body(object) }.finalizer.store(pointer, Ordering::Release);
}

unsafe extern "C-unwind" fn dealloc(this: *mut AnyObject, _: Sel) {
    let object = this.cast::<c_void>();
    // SAFETY: the object is being deallocated; its body is initialized and
    // nothing else refers to it any more.
    let (context, finalizer) = unsafe {
        let body = object.cast::<u8>().add(8).cast::<Body>();
        let finalizer = (*body).finalizer.load(Ordering::Acquire);
        let context = (*body).context.load(Ordering::Acquire);
        std::ptr::drop_in_place(body);
        (context, finalizer)
    };
    if !finalizer.is_null() {
        // SAFETY: the pointer was stored from a `Function`.
        let finalizer: Function = unsafe { std::mem::transmute::<*mut (), Function>(finalizer) };
        // SAFETY: the finalizer takes the object's context.
        unsafe { finalizer(context) };
    }
    // SAFETY: NSObject's -dealloc frees the object.
    unsafe {
        let _: () = msg_send![super(&*this, NSObject::class()), dealloc];
    }
}

fn load() {
    let mut builder = ClassBuilder::new(c"OS_dispatch_object", NSObject::class())
        .expect("sidestep: the dispatch object class is defined once");
    // SAFETY: the function matches -dealloc's convention.
    unsafe { builder.add_method(sel!(dealloc), dealloc as unsafe extern "C-unwind" fn(_, _)) };
    builder.register();
}
