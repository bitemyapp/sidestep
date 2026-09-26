//! Objects returned autoreleased: however the runtime hands them from
//! callee to caller, one the caller doesn't keep lives exactly as long as
//! an autoreleased object, until the pool it was returned into drains, and
//! one the caller keeps outlives that pool. Getting the same object back
//! later from a method that doesn't autorelease it doesn't change that.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::ffi;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};

use sidestep as _;

/// Counts its deallocations.
struct TrackedIvars {
    drops: Arc<AtomicUsize>,
}

impl Drop for TrackedIvars {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepAutoreleaseTracked"]
    #[ivars = TrackedIvars]
    struct Tracked;
);

struct FactoryIvars {
    drops: Arc<AtomicUsize>,
    made: Cell<usize>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepAutoreleaseFactory"]
    #[ivars = FactoryIvars]
    struct Factory;

    impl Factory {
        /// A new object nobody else owns, returned autoreleased.
        #[unsafe(method_id(make))]
        fn make(&self) -> Retained<Tracked> {
            self.ivars().made.set(self.ivars().made.get() + 1);
            let this = Tracked::alloc().set_ivars(TrackedIvars { drops: self.ivars().drops.clone() });
            unsafe { msg_send![super(this), init] }
        }
    }
);

fn factory() -> (Retained<Factory>, Arc<AtomicUsize>) {
    let drops = Arc::new(AtomicUsize::new(0));
    let this = Factory::alloc().set_ivars(FactoryIvars { drops: drops.clone(), made: Cell::new(0) });
    (unsafe { msg_send![super(this), init] }, drops)
}

/// Sends `make` without keeping the result: objc2 doesn't retain a raw
/// pointer.
fn make_unclaimed(factory: &Factory) -> *mut Tracked {
    unsafe { msg_send![factory, make] }
}

/// Sends `make` and keeps the result.
fn make_claimed(factory: &Factory) -> Retained<Tracked> {
    unsafe { msg_send![factory, make] }
}

fn dropped(drops: &AtomicUsize) -> usize {
    drops.load(Ordering::SeqCst)
}

#[test]
fn unclaimed_return_lives_until_its_pool_drains() {
    let (factory, drops) = factory();
    autoreleasepool(|_| {
        let obj = make_unclaimed(&factory);
        assert!(!obj.is_null());
        assert_eq!(dropped(&drops), 0);
        // Other messages, and other objects autoreleased after it, don't
        // end its life early.
        let other = NSObject::new();
        let _: *mut NSObject = unsafe { msg_send![&*other, self] };
        let _ = Retained::autorelease_ptr(other);
        assert_eq!(dropped(&drops), 0);
    });
    assert_eq!(dropped(&drops), 1);
}

#[test]
fn unclaimed_returns_in_a_row() {
    let (factory, drops) = factory();
    autoreleasepool(|_| {
        for _ in 0..3 {
            make_unclaimed(&factory);
        }
        assert_eq!(dropped(&drops), 0);
    });
    assert_eq!(dropped(&drops), 3);
}

#[test]
fn unclaimed_return_belongs_to_the_innermost_pool() {
    let (factory, drops) = factory();
    autoreleasepool(|_| {
        make_unclaimed(&factory);
        autoreleasepool(|_| {
            make_unclaimed(&factory);
            assert_eq!(dropped(&drops), 0);
        });
        // The inner pool took the second object with it, not the first.
        assert_eq!(dropped(&drops), 1);
        // A pool pushed and popped after an unclaimed return leaves it be.
        make_unclaimed(&factory);
        autoreleasepool(|_| {});
        assert_eq!(dropped(&drops), 1);
    });
    assert_eq!(dropped(&drops), 3);
}

#[test]
fn claimed_return_outlives_its_pool() {
    let (factory, drops) = factory();
    let kept = autoreleasepool(|_| make_claimed(&factory));
    assert_eq!(dropped(&drops), 0);
    drop(kept);
    assert_eq!(dropped(&drops), 1);
}

#[test]
fn claimed_and_unclaimed_mixed() {
    let (factory, drops) = factory();
    let kept = autoreleasepool(|_| {
        make_unclaimed(&factory);
        let kept = make_claimed(&factory);
        make_unclaimed(&factory);
        let kept_too = make_claimed(&factory);
        assert_eq!(dropped(&drops), 0);
        drop(kept_too);
        kept
    });
    // The two unclaimed ones and the dropped one are gone.
    assert_eq!(dropped(&drops), 3);
    drop(kept);
    assert_eq!(dropped(&drops), 4);
    assert_eq!(factory.ivars().made.get(), 4);
}

#[test]
fn claiming_a_different_object_retains_it() {
    let (factory, drops) = factory();
    autoreleasepool(|_| {
        let unclaimed = make_unclaimed(&factory);
        let other = make_claimed(&factory);
        // Taking a reference to the first object the usual way leaves it
        // in the pool as well.
        let extra = unsafe { Retained::retain_autoreleased(unclaimed) }.unwrap();
        drop(other);
        drop(extra);
        // The pool still holds its reference to the first object (and to
        // the second, unless it was handed over).
        assert!(dropped(&drops) <= 1);
    });
    // Neither leaked.
    assert_eq!(dropped(&drops), 2);
}

#[test]
fn unclaimed_return_released_when_its_thread_ends() {
    let (factory, drops) = factory();
    struct SendAnyway<T>(T);
    unsafe impl<T> Send for SendAnyway<T> {}
    let shared = SendAnyway(factory.clone());
    std::thread::spawn(move || {
        let shared = shared;
        make_unclaimed(&shared.0);
        make_unclaimed(&shared.0);
    })
    .join()
    .unwrap();
    assert_eq!(dropped(&drops), 2);
}

#[test]
fn autorelease_return_by_hand() {
    let (factory, drops) = factory();
    autoreleasepool(|_| {
        let obj = make_claimed(&factory);
        // What a method returning `obj` does, then what its caller does.
        let returned = Retained::autorelease_return(obj);
        let back = unsafe { Retained::retain_autoreleased(returned) }.unwrap();
        assert_eq!(dropped(&drops), 0);
        drop(back);
        // Whether the handoff happened or not, the object is gone once the
        // pool drains.
    });
    assert_eq!(dropped(&drops), 1);
}

/// Sidestep hands a claimed return value over without touching the pool,
/// so the caller holds the only new reference, when the method ends by
/// jumping to `objc_autoreleaseReturnValue` (a tail call) and the caller
/// claims the result straight after the call: what optimized builds of a
/// simple getter do. (Apple's runtime does the same when its optimization
/// applies, which isn't guaranteed.)
#[test]
#[cfg(all(not(target_vendor = "apple"), not(debug_assertions)))]
fn claimed_return_is_handed_over() {
    let drops = Arc::new(AtomicUsize::new(0));
    autoreleasepool(|_| {
        let holder = holder(&drops);
        let kept: Retained<Tracked> = unsafe { msg_send![&*holder, value] };
        // The holder's reference and the caller's.
        let count: usize = unsafe { msg_send![&*kept, retainCount] };
        assert_eq!(count, 2);
        drop(holder);
        drop(kept);
        assert_eq!(dropped(&drops), 1);
    });
}

/// Autoreleases an object of its own when it is deallocated.
struct ChainIvars {
    next: Option<Retained<Tracked>>,
}

impl Drop for ChainIvars {
    fn drop(&mut self) {
        if let Some(next) = self.next.take() {
            let _ = Retained::autorelease_ptr(next);
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepAutoreleaseChain"]
    #[ivars = ChainIvars]
    struct Chain;
);

#[test]
fn objects_autoreleased_while_a_pool_drains_go_with_it() {
    let (factory, drops) = factory();
    autoreleasepool(|_| {
        let this = Chain::alloc().set_ivars(ChainIvars { next: Some(make_claimed(&factory)) });
        let chain: Retained<Chain> = unsafe { msg_send![super(this), init] };
        let _ = Retained::autorelease_ptr(chain);
        assert_eq!(dropped(&drops), 0);
    });
    // Draining released the chain, whose deallocation autoreleased the
    // tracked object into the same pool, which released it too.
    assert_eq!(dropped(&drops), 1);
}

/// Makes a method call whose autoreleased result it doesn't keep when it is
/// deallocated.
struct DrainerIvars {
    factory: Retained<Factory>,
}

impl Drop for DrainerIvars {
    fn drop(&mut self) {
        make_unclaimed(&self.factory);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepAutoreleaseDrainer"]
    #[ivars = DrainerIvars]
    struct Drainer;
);

#[test]
fn unclaimed_return_while_a_pool_drains_goes_with_it() {
    let (factory, drops) = factory();
    autoreleasepool(|_| {
        autoreleasepool(|_| {
            let this = Drainer::alloc().set_ivars(DrainerIvars { factory: factory.clone() });
            let drainer: Retained<Drainer> = unsafe { msg_send![super(this), init] };
            let _ = Retained::autorelease_ptr(drainer);
        });
        // The object returned while the inner pool drained belonged to it.
        assert_eq!(dropped(&drops), 1);
    });
}

struct HolderIvars {
    value: Retained<Tracked>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepAutoreleaseHolder"]
    #[ivars = HolderIvars]
    struct Holder;

    impl Holder {
        /// Returned autoreleased.
        #[unsafe(method_id(value))]
        fn value(&self) -> Retained<Tracked> {
            self.ivars().value.clone()
        }

        /// Returned without a retain or an autorelease, like a
        /// collection's getter.
        #[unsafe(method(peek))]
        fn peek(&self) -> *mut Tracked {
            Retained::as_ptr(&self.ivars().value).cast_mut()
        }
    }
);

fn holder(drops: &Arc<AtomicUsize>) -> Retained<Holder> {
    let value = Tracked::alloc().set_ivars(TrackedIvars { drops: drops.clone() });
    let value: Retained<Tracked> = unsafe { msg_send![super(value), init] };
    let this = Holder::alloc().set_ivars(HolderIvars { value });
    unsafe { msg_send![super(this), init] }
}

/// An object the caller didn't keep, then got back from a method that
/// doesn't autorelease it: keeping it the second time takes a reference of
/// its own, and the pool's reference keeps the first pointer valid.
#[test]
fn a_later_plain_return_of_an_unclaimed_object_leaves_the_pool_its_reference() {
    let drops = Arc::new(AtomicUsize::new(0));
    autoreleasepool(|_| {
        let holder = holder(&drops);
        let raw: *mut Tracked = unsafe { msg_send![&*holder, value] };
        // Another message in between, whose result is kept.
        let other = NSObject::new();
        let _same: Retained<NSObject> = unsafe { msg_send![&*other, self] };
        let again: Retained<Tracked> = unsafe { msg_send![&*holder, peek] };
        assert_eq!(Retained::as_ptr(&again), raw.cast_const());
        drop(again);
        drop(holder);
        assert_eq!(dropped(&drops), 0, "the returned object died before its pool drained");
        // Still valid: the pool owns it.
        let count: usize = unsafe { msg_send![raw, retainCount] };
        assert!(count >= 1);
    });
    assert_eq!(dropped(&drops), 1);
}

/// The same with nothing in between: `-self` on the unclaimed object
/// returns it without an autorelease.
#[test]
fn a_plain_return_right_after_an_unclaimed_one_leaves_the_pool_its_reference() {
    let (factory, drops) = factory();
    autoreleasepool(|_| {
        let obj = make_unclaimed(&factory);
        let again: Retained<Tracked> = unsafe { msg_send![obj, self] };
        drop(again);
        assert_eq!(dropped(&drops), 0, "the returned object died before its pool drained");
    });
    assert_eq!(dropped(&drops), 1);
}

/// `objc_retainAutoreleaseReturnValue`, what a method returning an object
/// it only borrowed ends with: the object lives until the pool drains, or
/// passes to a caller that keeps it.
#[test]
fn retain_autorelease_return_value() {
    let (factory, drops) = factory();
    autoreleasepool(|_| {
        let obj = make_claimed(&factory);
        let ptr = Retained::as_ptr(&obj).cast_mut().cast::<AnyObject>();
        let returned = unsafe { ffi::objc_retainAutoreleaseReturnValue(ptr) };
        assert_eq!(returned, ptr);
        drop(obj);
        assert_eq!(dropped(&drops), 0);
    });
    assert_eq!(dropped(&drops), 1);

    autoreleasepool(|_| {
        let obj = make_claimed(&factory);
        let ptr = Retained::as_ptr(&obj).cast_mut().cast::<AnyObject>();
        let kept = unsafe { Retained::retain_autoreleased(ffi::objc_retainAutoreleaseReturnValue(ptr)) }.unwrap();
        drop(obj);
        assert_eq!(dropped(&drops), 1);
        drop(kept);
    });
    assert_eq!(dropped(&drops), 2);
}
