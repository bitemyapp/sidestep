//! `NSProxy`: a root class of its own. It implements reference counting,
//! identity (`-class`, `-hash`, `-isEqual:`, `-isProxy`) and
//! `-description` itself, and forwards everything else, `-isKindOfClass:`,
//! `-respondsToSelector:` and `-conformsToProtocol:` included, through
//! `-methodSignatureForSelector:` and `-forwardInvocation:`, which a
//! subclass overrides.
//!
//! Messages go through `objc_msgSend`: objc2 checks in debug builds that
//! a method exists before sending it, and `-methodForSelector:` is itself
//! forwarded by a proxy.

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::rc::{PartialInit, Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Encode, Encoding, ProtocolType, define_class, ffi, msg_send, sel};
use objc2_foundation::{NSInvocation, NSMethodSignature, NSPoint, NSProxy, NSString};

use sidestep as _;

/// Returned through memory on every architecture.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct Wide {
    v: [i64; 5],
}

unsafe impl Encode for Wide {
    const ENCODING: Encoding = Encoding::Struct("SidestepProxyWide", &[<[i64; 5]>::ENCODING]);
}

define_class!(
    /// Counts its deallocations in its test's counter.
    #[unsafe(super(NSObject))]
    #[name = "SidestepProxyTarget"]
    #[ivars = &'static AtomicUsize]
    struct Target;

    impl Target {
        #[unsafe(method(value))]
        fn value(&self) -> i64 {
            42
        }

        #[unsafe(method(point:))]
        fn point(&self, p: NSPoint) -> NSPoint {
            NSPoint::new(p.y, p.x)
        }

        #[unsafe(method(wide:))]
        fn wide(&self, x: i64) -> Wide {
            Wide { v: [x, x + 1, x + 2, x + 3, x + 4] }
        }
    }
);

impl Drop for Target {
    fn drop(&mut self) {
        self.ivars().fetch_add(1, Ordering::SeqCst);
    }
}

fn target(deallocs: &'static AtomicUsize) -> Retained<Target> {
    let this = Target::alloc().set_ivars(deallocs);
    unsafe { msg_send![super(this), init] }
}

struct ForwarderIvars {
    target: Retained<Target>,
    log: RefCell<Vec<String>>,
}

define_class!(
    /// Forwards everything to its target, logging what it forwards.
    #[unsafe(super(NSProxy))]
    #[name = "SidestepProxyForwarder"]
    #[ivars = ForwarderIvars]
    struct Forwarder;

    impl Forwarder {
        #[unsafe(method_id(methodSignatureForSelector:))]
        fn method_signature(&self, sel: Sel) -> Option<Retained<NSMethodSignature>> {
            unsafe { msg_send![&*self.ivars().target, methodSignatureForSelector: sel] }
        }

        #[unsafe(method(forwardInvocation:))]
        fn forward_invocation(&self, inv: &NSInvocation) {
            let sel = unsafe { inv.selector() };
            self.ivars().log.borrow_mut().push(sel.name().to_str().unwrap().to_owned());
            unsafe { inv.invokeWithTarget(&self.ivars().target) };
        }
    }
);

/// A proxy for `target`. NSProxy has no `-init` (it would be forwarded),
/// so the proxy is used as allocated.
fn forwarder(target: Retained<Target>) -> Retained<Forwarder> {
    let mut this = Forwarder::alloc().set_ivars(ForwarderIvars { target, log: RefCell::default() });
    let ptr = PartialInit::as_mut_ptr(&mut this);
    std::mem::forget(this);
    unsafe { Retained::from_raw(ptr) }.expect("an allocated proxy")
}

define_class!(
    /// Has signatures but doesn't forward: NSProxy's own
    /// `-forwardInvocation:` gets the invocation.
    #[unsafe(super(NSProxy))]
    #[name = "SidestepProxySignaturesOnly"]
    struct SignaturesOnly;

    impl SignaturesOnly {
        #[unsafe(method_id(methodSignatureForSelector:))]
        fn method_signature(&self, _sel: Sel) -> Option<Retained<NSMethodSignature>> {
            unsafe { NSMethodSignature::signatureWithObjCTypes(NonNull::new(c"q@:".as_ptr().cast_mut()).unwrap()) }
        }
    }
);

fn send<F: Copy>() -> F {
    assert_eq!(size_of::<F>(), size_of::<unsafe extern "C-unwind" fn()>());
    let f: unsafe extern "C-unwind" fn() = ffi::objc_msgSend;
    unsafe { std::mem::transmute_copy(&f) }
}

type Id = *mut AnyObject;

fn id<T: ?Sized>(obj: &T) -> Id {
    (obj as *const T).cast::<AnyObject>().cast_mut()
}

fn class_id(cls: &AnyClass) -> Id {
    (cls as *const AnyClass).cast::<AnyObject>().cast_mut()
}

fn send_bool_class(receiver: Id, sel: Sel, cls: &AnyClass) -> bool {
    let f: unsafe extern "C-unwind" fn(Id, Sel, *const AnyClass) -> bool = send();
    unsafe { f(receiver, sel, cls) }
}

fn send_bool_sel(receiver: Id, sel: Sel, arg: Sel) -> bool {
    let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> bool = send();
    unsafe { f(receiver, sel, arg) }
}

fn send_id(receiver: Id, sel: Sel) -> Id {
    let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = send();
    unsafe { f(receiver, sel) }
}

fn send_usize(receiver: Id, sel: Sel) -> usize {
    let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = send();
    unsafe { f(receiver, sel) }
}

fn string(obj: Id) -> String {
    unsafe { &*obj.cast::<NSString>() }.to_string()
}

fn class_name(obj: Id) -> String {
    unsafe { std::ffi::CStr::from_ptr(ffi::class_getName(obj.cast())) }.to_str().unwrap().to_owned()
}

/// The reason an operation fails with: Apple raises an NSException;
/// Sidestep panics with the same message.
fn failure(f: impl FnOnce()) -> String {
    #[cfg(target_vendor = "apple")]
    {
        match objc2::exception::catch(AssertUnwindSafe(f)) {
            Ok(()) => panic!("expected an exception"),
            Err(Some(e)) => {
                let reason: Retained<NSString> = unsafe { msg_send![&*e, reason] };
                reason.to_string()
            }
            Err(None) => panic!("nil exception"),
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let e = std::panic::catch_unwind(AssertUnwindSafe(f)).expect_err("expected a panic");
        match e.downcast::<String>() {
            Ok(s) => *s,
            Err(e) => e.downcast::<&str>().map(|s| s.to_string()).unwrap_or_default(),
        }
    }
}

fn logged(proxy: &Forwarder) -> Vec<String> {
    proxy.ivars().log.take()
}

#[test]
fn proxies_forward_what_they_do_not_implement() {
    static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
    let proxy = forwarder(target(&DEALLOCS));
    let p = id(&*proxy);
    let value: unsafe extern "C-unwind" fn(Id, Sel) -> i64 = send();
    assert_eq!(unsafe { value(p, sel!(value)) }, 42);
    let point: unsafe extern "C-unwind" fn(Id, Sel, NSPoint) -> NSPoint = send();
    assert_eq!(unsafe { point(p, sel!(point:), NSPoint::new(1.0, 2.0)) }, NSPoint::new(2.0, 1.0));
    #[cfg(target_arch = "x86_64")]
    let wide: unsafe extern "C-unwind" fn(Id, Sel, i64) -> Wide = {
        let f: unsafe extern "C-unwind" fn() = ffi::objc_msgSend_stret;
        unsafe { std::mem::transmute(f) }
    };
    #[cfg(not(target_arch = "x86_64"))]
    let wide: unsafe extern "C-unwind" fn(Id, Sel, i64) -> Wide = send();
    assert_eq!(unsafe { wide(p, sel!(wide:), 7) }, Wide { v: [7, 8, 9, 10, 11] });
    assert_eq!(logged(&proxy), ["value", "point:", "wide:"]);
}

/// Class membership and the messages a target answers are the target's.
#[test]
fn proxies_forward_introspection() {
    static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
    let proxy = forwarder(target(&DEALLOCS));
    let p = id(&*proxy);
    assert!(send_bool_class(p, sel!(isKindOfClass:), Target::class()));
    assert!(send_bool_class(p, sel!(isKindOfClass:), NSObject::class()));
    assert!(!send_bool_class(p, sel!(isKindOfClass:), Forwarder::class()));
    assert!(!send_bool_class(p, sel!(isKindOfClass:), NSProxy::class()));
    assert!(send_bool_class(p, sel!(isMemberOfClass:), Target::class()));
    assert!(send_bool_sel(p, sel!(respondsToSelector:), sel!(value)));
    assert!(!send_bool_sel(p, sel!(respondsToSelector:), sel!(sidestepNoSuchMethod)));
    let proto = <dyn NSObjectProtocol>::protocol().unwrap();
    let conforms: unsafe extern "C-unwind" fn(Id, Sel, *const AnyProtocol) -> bool = send();
    assert!(unsafe { conforms(p, sel!(conformsToProtocol:), proto) });
    assert_eq!(
        logged(&proxy),
        [
            "isKindOfClass:",
            "isKindOfClass:",
            "isKindOfClass:",
            "isKindOfClass:",
            "isMemberOfClass:",
            "respondsToSelector:",
            "respondsToSelector:",
            "conformsToProtocol:"
        ]
    );
    // -performSelector: sends the message to the proxy, which forwards it.
    let perform: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> i64 = send();
    assert_eq!(unsafe { perform(p, sel!(performSelector:), sel!(value)) }, 42);
    assert_eq!(logged(&proxy), ["value"]);
}

/// Identity and description are the proxy's own.
#[test]
fn proxies_answer_for_themselves() {
    static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
    let target = target(&DEALLOCS);
    let proxy = forwarder(target.clone());
    let p = id(&*proxy);
    assert_eq!(send_usize(p, sel!(isProxy)) & 0xff, 1);
    assert_eq!(send_id(p, sel!(class)), class_id(Forwarder::class()));
    assert_eq!(send_id(p, sel!(superclass)), class_id(NSProxy::class()));
    assert_eq!(send_id(p, sel!(self)), p);
    assert_eq!(send_usize(p, sel!(hash)), p as usize);
    let equal: unsafe extern "C-unwind" fn(Id, Sel, Id) -> bool = send();
    assert!(unsafe { equal(p, sel!(isEqual:), p) });
    assert!(!unsafe { equal(p, sel!(isEqual:), id(&*target)) });
    autoreleasepool(|_| {
        let description = string(send_id(p, sel!(description)));
        assert!(description.starts_with("<SidestepProxyForwarder: 0x"), "{description}");
        assert_eq!(string(send_id(p, sel!(debugDescription))), description);
    });
    assert_eq!(send_usize(p, sel!(retainCount)), 1);
    assert!(logged(&proxy).is_empty());
}

#[test]
fn proxies_are_counted_and_freed() {
    static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
    let proxy = forwarder(target(&DEALLOCS));
    let extra = proxy.clone();
    assert_eq!(send_usize(id(&*proxy), sel!(retainCount)), 2);
    drop(extra);
    assert_eq!(DEALLOCS.load(Ordering::SeqCst), 0);
    // Freeing the proxy drops its instance variables, the target with them.
    drop(proxy);
    assert_eq!(DEALLOCS.load(Ordering::SeqCst), 1);
}

#[test]
fn proxy_classes_answer_as_classes() {
    let cls = class_id(Forwarder::class());
    assert_eq!(send_id(cls, sel!(class)), cls);
    assert_eq!(send_id(cls, sel!(superclass)), class_id(NSProxy::class()));
    assert!(send_id(class_id(NSProxy::class()), sel!(superclass)).is_null());
    assert!(send_bool_sel(cls, sel!(respondsToSelector:), sel!(alloc)));
    assert!(!send_bool_sel(cls, sel!(respondsToSelector:), sel!(value)));
    autoreleasepool(|_| assert_eq!(string(send_id(cls, sel!(description))), "SidestepProxyForwarder"));
    assert_eq!(Forwarder::class().superclass(), Some(NSProxy::class()));
    assert!(NSProxy::class().superclass().is_none());
}

#[test]
fn plain_proxies_refuse_to_forward() {
    let proxy = send_id(class_id(NSProxy::class()), sel!(alloc));
    assert!(!proxy.is_null());
    assert_eq!(class_name(send_id(proxy, sel!(class))), "NSProxy");
    let reason = failure(|| {
        send_usize(proxy, sel!(value));
    });
    assert_eq!(reason, "*** -[NSProxy methodSignatureForSelector:] called!");
    let reason = failure(|| {
        send_bool_sel(proxy, sel!(respondsToSelector:), sel!(value));
    });
    assert_eq!(reason, "*** -[NSProxy methodSignatureForSelector:] called!");
    autoreleasepool(|_| {
        let description = string(send_id(proxy, sel!(description)));
        assert!(description.starts_with("<NSProxy: 0x"), "{description}");
    });
    let signatures_only = send_id(class_id(SignaturesOnly::class()), sel!(alloc));
    let reason = failure(|| {
        send_usize(signatures_only, sel!(value));
    });
    assert_eq!(reason, "*** -[NSProxy forwardInvocation:] called!");
    unsafe {
        ffi::objc_release(proxy);
        ffi::objc_release(signatures_only);
    }
}
