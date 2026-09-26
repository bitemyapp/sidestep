//! Raising: `objc_exception_throw` and the collection-mutation check.
//!
//! Sidestep has no Objective-C exception unwinding: Clang's `@try`/`@catch`
//! needs a personality routine, and objc2's `exception` feature compiles
//! Objective-C with Clang at build time, which Sidestep's builds don't
//! have. What Apple's runtime raises as an exception, Sidestep raises as a
//! Rust panic instead, which unwinds through objc2's `C-unwind` frames
//! into the Rust caller and can be caught with `catch_unwind`.

use std::sync::atomic::{AtomicPtr, Ordering};

use crate::object::{Object, isa};

type Id = *mut Object;

/// Raises `exception` as a panic, whose message names its class. The
/// exception object is kept alive (leaked), as nothing can catch it as an
/// object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_exception_throw(exception: Id) -> ! {
    if exception.is_null() {
        panic!("objc_exception_throw: nil exception");
    }
    // SAFETY: the caller passes a live object.
    let class = unsafe { isa(exception) }.instance_class().name();
    panic!("uncaught Objective-C exception: <{}: {exception:p}>", class.to_string_lossy());
}

type MutationHandler = unsafe extern "C-unwind" fn(Id);

/// The handler `objc_setEnumerationMutationHandler` set, or null.
static MUTATION_HANDLER: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// Called by a collection whose contents changed while a fast enumeration
/// was running over it. Calls the handler if one is set, and otherwise
/// panics with the message Apple's Foundation raises.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_enumerationMutation(obj: Id) {
    let handler = MUTATION_HANDLER.load(Ordering::Acquire);
    if !handler.is_null() {
        // SAFETY: only handlers are stored, by the setter below.
        let handler: MutationHandler = unsafe { std::mem::transmute(handler) };
        // SAFETY: the caller passes the collection.
        unsafe { handler(obj) };
        return;
    }
    let class = if obj.is_null() {
        c"nil"
    } else {
        // SAFETY: the caller passes a live collection.
        unsafe { isa(obj) }.instance_class().name()
    };
    panic!("*** Collection <{}: {obj:p}> was mutated while being enumerated.", class.to_string_lossy());
}

#[unsafe(no_mangle)]
pub extern "C" fn objc_setEnumerationMutationHandler(handler: Option<MutationHandler>) {
    let handler = handler.map_or(std::ptr::null_mut(), |h| h as *mut ());
    MUTATION_HANDLER.store(handler, Ordering::Release);
}
