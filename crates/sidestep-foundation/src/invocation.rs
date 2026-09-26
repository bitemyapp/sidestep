//! `NSMethodSignature` and `NSInvocation`, and forwarding messages through
//! `-forwardInvocation:`.
//!
//! A signature holds the runtime's `call::Signature`: the method's types,
//! parsed once and laid out for the platform's calling convention. An
//! invocation holds one buffer with a place for each argument and the
//! return value, where the layout puts them, and `-invoke` calls the
//! target's implementation with them (`Signature::call`). Signatures are
//! immutable and shared, as Foundation's are: the one for a method is made
//! once and handed out again.
//!
//! Forwarding: a category on `NSObject` adds
//! `-methodSignatureForSelector:`, `+instanceMethodSignatureForSelector:`
//! and `-forwardInvocation:` (which reports the selector unrecognized),
//! and installs [`forward`] as the runtime's forward handler. The runtime
//! calls it for a message the receiver doesn't implement, after
//! `-forwardingTargetForSelector:`, with the message's registers: it asks
//! the receiver for the method's signature, builds an invocation from the
//! registers, sends it to `-forwardInvocation:`, and writes its return
//! value back for the sender. `NSProxy` (in the runtime) forwards
//! everything this way; another category gives its class the same class
//! methods as `NSObject`'s, in place of the instance methods that raise.
//!
//! `-retainArguments` retains the target and object arguments, copies
//! blocks and C strings, and does the same for arguments set afterwards
//! and for the return value. As in Foundation, a value replaced isn't
//! released until the invocation goes, which releases them all. A
//! forwarded message's invocation that owns its return value that way is
//! autoreleased, not freed, once `-forwardInvocation:` returns, so the
//! value reaches the sender alive.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::NonNull;
use std::sync::{Arc, LazyLock, RwLock};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, Imp, NSObject, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, ffi, msg_send, sel};
use objc2_foundation::{NSInteger, NSInvocation, NSMethodSignature, NSUInteger};
use sidestep_runtime::call::{Frame, Signature, Value, ValueKind};

use crate::util::inherits;

sidestep_runtime::static_class!(pub NSMETHODSIGNATURE, NSMETHODSIGNATURE_META = "NSMethodSignature", || {
    let _ = NSMethodSignatureImpl::class();
});

sidestep_runtime::static_class!(pub NSINVOCATION, NSINVOCATION_META = "NSInvocation", || {
    let _ = NSInvocationImpl::class();
});

sidestep_runtime::category!("NSObject"(SidestepForwarding), |category| {
    sidestep_runtime::call::set_forward_handler(forward);
    // SAFETY: each function's signature matches the selector's convention.
    unsafe {
        category.add_method(
            sel!(methodSignatureForSelector:),
            method_signature_for_selector as unsafe extern "C-unwind" fn(_, _, _) -> _,
        );
        category.add_method(sel!(forwardInvocation:), forward_invocation as unsafe extern "C-unwind" fn(_, _, _));
        category.add_class_method(
            sel!(instanceMethodSignatureForSelector:),
            instance_method_signature_for_selector as unsafe extern "C-unwind" fn(_, _, _) -> _,
        );
    }
});

// NSProxy's instance methods raise, and are its class's too (a root
// class's metaclass inherits from it): its class methods get NSObject's.
sidestep_runtime::category!("NSProxy"(SidestepProxyClassForwarding), |category| {
    // SAFETY: each function's signature matches the selector's convention.
    unsafe {
        category.add_class_method(
            sel!(methodSignatureForSelector:),
            class_method_signature_for_selector as unsafe extern "C-unwind" fn(_, _, _) -> _,
        );
        category.add_class_method(
            sel!(forwardInvocation:),
            class_forward_invocation as unsafe extern "C-unwind" fn(_, _, _),
        );
        category.add_class_method(
            sel!(instanceMethodSignatureForSelector:),
            instance_method_signature_for_selector as unsafe extern "C-unwind" fn(_, _, _) -> _,
        );
    }
});

pub(crate) struct SignatureIvars {
    layout: Arc<Signature>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSMethodSignature"]
    #[ivars = SignatureIvars]
    pub(crate) struct NSMethodSignatureImpl;

    impl NSMethodSignatureImpl {
        #[unsafe(method(signatureWithObjCTypes:))]
        fn signature_with_types(types: NonNull<c_char>) -> *mut Self {
            // SAFETY: the caller passes a C string.
            let types = unsafe { CStr::from_ptr(types.as_ptr()) };
            parse(types.to_bytes()).map_or(std::ptr::null_mut(), Retained::autorelease_return)
        }

        /// A signature needs types: Foundation's `-init` gives nil.
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Option<Retained<Self>> {
            drop(this);
            None
        }

        #[unsafe(method(numberOfArguments))]
        fn number_of_arguments(&self) -> NSUInteger {
            self.ivars().layout.arguments().len()
        }

        #[unsafe(method(getArgumentTypeAtIndex:))]
        fn argument_type(&self, index: NSUInteger) -> NonNull<c_char> {
            let arguments = self.ivars().layout.arguments();
            let Some(arg) = arguments.get(index) else {
                panic!(
                    "-[NSMethodSignature getArgumentTypeAtIndex:]: index ({index}) out of bounds [0, {}]",
                    arguments.len() as isize - 1
                );
            };
            c_text(arg.types())
        }

        #[unsafe(method(frameLength))]
        fn frame_length(&self) -> NSUInteger {
            self.ivars().layout.frame_length()
        }

        #[unsafe(method(isOneway))]
        fn is_oneway(&self) -> bool {
            self.ivars().layout.is_oneway()
        }

        #[unsafe(method(methodReturnType))]
        fn method_return_type(&self) -> NonNull<c_char> {
            c_text(self.ivars().layout.return_value().types())
        }

        #[unsafe(method(methodReturnLength))]
        fn method_return_length(&self) -> NSUInteger {
            self.ivars().layout.return_value().size()
        }

        /// Signatures with the same types are equal, offsets aside (see
        /// [`same_type`]).
        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| self.same_types(other))
        }

        /// Hashes what [`same_type`] compares in every case: an object or
        /// block type without its class name or block signature.
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            let layout = &self.ivars().layout;
            let hash = |value: &Value| crate::string::hash_bytes(compared(value));
            layout.arguments().iter().fold(hash(layout.return_value()), |h, a| h.rotate_left(5) ^ hash(a))
        }
    }
);

impl NSMethodSignatureImpl {
    fn same_types(&self, other: &AnyObject) -> bool {
        if !inherits(other, &NSMETHODSIGNATURE) {
            return false;
        }
        // SAFETY: an NSMethodSignature has this class's storage.
        let other = unsafe { &*(other as *const AnyObject).cast::<Self>() };
        let (a, b) = (&self.ivars().layout, &other.ivars().layout);
        same_type(a.return_value(), b.return_value())
            && a.arguments().len() == b.arguments().len()
            && a.arguments().iter().zip(b.arguments()).all(|(x, y)| same_type(x, y))
    }
}

/// Whether two values have the same type, as Foundation compares
/// signatures: the same encoding, except that an object type without a
/// class name (`@`) matches one with (`@"NSString"`), and a block type
/// without a signature (`@?`) one with (`@?<v@?>`). Two class names, or
/// two block signatures, must be the same.
fn same_type(a: &Value, b: &Value) -> bool {
    let (x, y) = (a.types().to_bytes(), b.types().to_bytes());
    if x == y {
        return true;
    }
    let (bare_x, bare_y) = (compared(a), compared(b));
    a.kind() == b.kind() && bare_x == bare_y && (bare_x.len() == x.len() || bare_y.len() == y.len())
}

/// The part of a value's encoding every equal type shares: an object or
/// block type's qualifiers and `@` or `@?`, or all of any other type.
fn compared(value: &Value) -> &[u8] {
    let types = value.types().to_bytes();
    // Qualifiers come before the `@`, and are never one.
    let at = types.iter().position(|&c| c == b'@');
    match (value.kind(), at) {
        (ValueKind::Object, Some(at)) => &types[..=at],
        (ValueKind::Block, Some(at)) => &types[..(at + 2).min(types.len())],
        _ => types,
    }
}

/// A C string's pointer as the bindings pass it.
fn c_text(text: &CStr) -> NonNull<c_char> {
    NonNull::new(text.as_ptr().cast_mut()).expect("a C string")
}

/// A new signature for `types`, or `None` for an empty encoding. Raises,
/// as Foundation does, for types it can't lay out.
fn parse(types: &[u8]) -> Option<Retained<NSMethodSignatureImpl>> {
    let layout = match Signature::parse(types) {
        Ok(layout) => layout?,
        Err(e) if e.union => panic!(
            "+[NSMethodSignature signatureWithObjCTypes:]: unsupported type encoding spec '{}' in '{}'",
            e.spec, e.at
        ),
        Err(e) => {
            panic!(
                "NSGetSizeAndAlignment(): unsupported type encoding spec '{}' at '{}' in '{}'",
                e.spec, e.at, e.within
            )
        }
    };
    // Sending +alloc through the binding loads the class on first use.
    let this = NSMethodSignature::alloc();
    // SAFETY: NSMethodSignature's class is NSMethodSignatureImpl, and an
    // `Allocated` is a pointer to its object whatever its type parameter.
    let this = unsafe { std::mem::transmute::<Allocated<NSMethodSignature>, Allocated<NSMethodSignatureImpl>>(this) };
    let this = this.set_ivars(SignatureIvars { layout: Arc::new(layout) });
    // SAFETY: NSObject's initializer.
    Some(unsafe { msg_send![super(this), init] })
}

/// The signatures of methods, by their type encoding's address: method
/// encodings live as long as the program, so each is parsed once and its
/// signature kept.
static METHOD_SIGNATURES: LazyLock<RwLock<HashMap<usize, Shared>>> = LazyLock::new(Default::default);

/// A signature shared between threads, which its immutability allows.
#[derive(Clone, Copy)]
struct Shared(*mut NSMethodSignatureImpl);

// SAFETY: signatures never change after they are made, and reference
// counting is atomic.
unsafe impl Send for Shared {}
// SAFETY: as above.
unsafe impl Sync for Shared {}

/// The signature of the method `sel` names on `cls`, unretained, or null
/// if there is none. For instances (`cls` not a metaclass), a method
/// declared by a protocol the class adopts counts too, implemented or not,
/// as in Foundation: a forwarder can then take the optional protocol
/// messages nothing implements. Foundation looks at the protocols first,
/// which only makes a difference when a class implements a protocol's
/// method with other types than the protocol declares; the method comes
/// first here, so the common case costs no walk through the protocols.
fn signature_of_method(cls: &AnyClass, sel: Sel) -> *mut AnyObject {
    let types = match cls.instance_method(sel) {
        // SAFETY: a method from the runtime.
        Some(method) => unsafe { ffi::method_getTypeEncoding(method) },
        None if !cls.is_metaclass() => match declared_by_protocols(cls, sel) {
            Some(types) => types,
            None => return std::ptr::null_mut(),
        },
        None => return std::ptr::null_mut(),
    };
    // Method encodings and protocols' method descriptions are never freed.
    if let Some(&Shared(sig)) = METHOD_SIGNATURES.read().unwrap().get(&(types as usize)) {
        return sig.cast();
    }
    // SAFETY: as above.
    let Some(sig) = parse(unsafe { CStr::from_ptr(types) }.to_bytes()) else { return std::ptr::null_mut() };
    let mut cache = METHOD_SIGNATURES.write().unwrap();
    // Kept for the rest of the program; a signature another thread made
    // meanwhile wins, and this one goes.
    cache.entry(types as usize).or_insert_with(|| Shared(Retained::into_raw(sig))).0.cast()
}

/// The types of the instance method `sel` as a protocol that `cls` or a
/// superclass adopts declares it, directly or through the protocols it
/// adopts in turn, required or optional.
fn declared_by_protocols(cls: &AnyClass, sel: Sel) -> Option<*const c_char> {
    let mut class = Some(cls);
    while let Some(c) = class {
        if let Some(types) = c.adopted_protocols().iter().find_map(|p| declared_by(p, sel)) {
            return Some(types);
        }
        class = c.superclass();
    }
    None
}

fn declared_by(proto: &AnyProtocol, sel: Sel) -> Option<*const c_char> {
    for required in [Bool::YES, Bool::NO] {
        // SAFETY: a protocol from the runtime.
        let description = unsafe { ffi::protocol_getMethodDescription(proto, sel, required, Bool::YES) };
        if description.name.is_some() && !description.types.is_null() {
            return Some(description.types);
        }
    }
    proto.adopted_protocols().iter().find_map(|p| declared_by(p, sel))
}

/// `-[NSObject methodSignatureForSelector:]`: the signature of the
/// receiver's method, a class method when the receiver is a class.
unsafe extern "C-unwind" fn method_signature_for_selector(this: &AnyObject, _: Sel, sel: Sel) -> *mut AnyObject {
    signature_of_method(this.class(), sel)
}

/// `+[NSObject instanceMethodSignatureForSelector:]`.
unsafe extern "C-unwind" fn instance_method_signature_for_selector(cls: &AnyClass, _: Sel, sel: Sel) -> *mut AnyObject {
    signature_of_method(cls, sel)
}

/// `+[NSProxy methodSignatureForSelector:]`: the signature of the class's
/// class method.
unsafe extern "C-unwind" fn class_method_signature_for_selector(cls: &AnyClass, _: Sel, sel: Sel) -> *mut AnyObject {
    signature_of_method(cls.metaclass(), sel)
}

/// `+[NSProxy forwardInvocation:]`: nothing takes the message.
unsafe extern "C-unwind" fn class_forward_invocation(cls: &AnyClass, cmd: Sel, invocation: *mut AnyObject) {
    // SAFETY: a class is an object; its `-forwardInvocation:` is NSObject's.
    unsafe { forward_invocation(&*(cls as *const AnyClass).cast::<AnyObject>(), cmd, invocation) }
}

/// `-[NSObject forwardInvocation:]`: nothing takes the message.
unsafe extern "C-unwind" fn forward_invocation(this: &AnyObject, _: Sel, invocation: *mut AnyObject) {
    // SAFETY: -selector takes nothing and returns a selector, or none.
    let sel: Option<Sel> = unsafe { msg_send![invocation, selector] };
    sidestep_runtime::call::does_not_recognize(
        this,
        sel.expect("-forwardInvocation: with an invocation of no selector"),
    );
}

/// The runtime's forward handler: `[receiver forwardInvocation:]` with an
/// invocation made from the message's registers, per
/// `[receiver methodSignatureForSelector:sel]`, whose return value goes
/// back to the sender.
unsafe fn forward(receiver: &AnyObject, sel: Sel, frame: &mut Frame) {
    // A root class of someone else's without the method would forward the
    // question itself, for ever.
    if !receiver.class().responds_to(sel!(methodSignatureForSelector:)) {
        sidestep_runtime::call::does_not_recognize(receiver, sel);
    }
    // SAFETY: the method takes a selector and returns a signature.
    let signature: Option<Retained<NSMethodSignature>> =
        unsafe { msg_send![receiver, methodSignatureForSelector: sel] };
    let Some(signature) = signature else { sidestep_runtime::call::does_not_recognize(receiver, sel) };
    let invocation = invocation_with_signature(&signature);
    let ivars = invocation.ivars();
    // SAFETY: the frame holds a message sent with `sel`, whose signature
    // the receiver vouches for; the buffer is laid out for it.
    unsafe { ivars.layout.read_arguments(frame, ivars.values.ptr()) };
    // SAFETY: the method takes an invocation and returns nothing.
    let _: () = unsafe { msg_send![receiver, forwardInvocation: &*invocation] };
    // SAFETY: as above.
    unsafe { ivars.layout.write_return(frame, ivars.values.ptr()) };
    // A retained invocation owns its return value (an object, a block copy
    // or a C string copy): it lives until the pool drains, as Foundation's
    // does, so the sender doesn't get a value freed with it.
    let owns_return = ivars.retained.get()
        && matches!(ivars.layout.return_value().kind(), ValueKind::Object | ValueKind::Block | ValueKind::CString);
    if owns_return {
        let _ = Retained::autorelease_ptr(invocation);
    }
}

/// A zeroed buffer, 16-byte aligned, that methods of an invocation read and
/// write through raw pointers only, so a method called by `-invoke` may
/// change the invocation it was called from.
struct Buffer {
    ptr: NonNull<u8>,
    len: usize,
}

impl Buffer {
    fn new(len: usize) -> Buffer {
        let layout = std::alloc::Layout::from_size_align(len.max(16), 16).expect("an argument buffer's layout");
        // SAFETY: a non-zero size.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        let Some(ptr) = NonNull::new(ptr) else { std::alloc::handle_alloc_error(layout) };
        Buffer { ptr, len: len.max(16) }
    }

    fn ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout.
        unsafe { std::alloc::dealloc(self.ptr.as_ptr(), std::alloc::Layout::from_size_align_unchecked(self.len, 16)) };
    }
}

pub(crate) struct InvocationIvars {
    signature: Retained<NSMethodSignature>,
    layout: Arc<Signature>,
    values: Buffer,
    retained: Cell<bool>,
    /// Once arguments are retained, every object the invocation has
    /// retained and every block copy it has made, released when it goes.
    /// A value replaced stays here, as in Foundation, so a pointer read
    /// from the invocation earlier stays good.
    kept: RefCell<Vec<NonNull<AnyObject>>>,
    /// Likewise the copies of C strings.
    strings: RefCell<Vec<CString>>,
}

impl Drop for InvocationIvars {
    fn drop(&mut self) {
        for &value in self.kept.get_mut().iter() {
            // SAFETY: a retained object or a block copy, which the
            // invocation owns; blocks are released like objects.
            unsafe { ffi::objc_release(value.as_ptr()) };
        }
    }
}

/// An argument, by index, or the return value.
#[derive(Clone, Copy)]
enum Slot {
    Argument(usize),
    Return,
}

impl Slot {
    fn value(self, layout: &Signature) -> &Value {
        match self {
            Slot::Argument(i) => &layout.arguments()[i],
            Slot::Return => layout.return_value(),
        }
    }
}

/// # Safety
/// `at` must hold a pointer.
unsafe fn read_pointer(at: *const u8) -> *mut AnyObject {
    // SAFETY: guaranteed by the caller; the buffer places pointers aligned.
    unsafe { at.cast::<*mut AnyObject>().read() }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSInvocation"]
    #[ivars = InvocationIvars]
    pub(crate) struct NSInvocationImpl;

    impl NSInvocationImpl {
        #[unsafe(method(invocationWithMethodSignature:))]
        fn with_signature(signature: &NSMethodSignature) -> *mut Self {
            Retained::autorelease_return(invocation_with_signature(signature))
        }

        /// An invocation needs a signature: Foundation's `-init` gives nil.
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Option<Retained<Self>> {
            drop(this);
            None
        }

        #[unsafe(method_id(methodSignature))]
        fn method_signature(&self) -> Retained<NSMethodSignature> {
            self.ivars().signature.clone()
        }

        #[unsafe(method(retainArguments))]
        fn retain_arguments(&self) {
            let ivars = self.ivars();
            if ivars.retained.replace(true) {
                return;
            }
            let slots = (0..ivars.layout.arguments().len()).map(Slot::Argument).chain([Slot::Return]);
            for slot in slots {
                // SAFETY: each slot's current value, retained in place.
                unsafe { self.retain_in_place(slot) };
            }
        }

        #[unsafe(method(argumentsRetained))]
        fn arguments_retained(&self) -> bool {
            self.ivars().retained.get()
        }

        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> {
            let target = self.pointer(self.slot(0, "getArgument:atIndex:"));
            // SAFETY: the target is an object, retained for the caller.
            unsafe { Retained::retain(target) }
        }

        #[unsafe(method(setTarget:))]
        fn set_target(&self, target: Option<&AnyObject>) {
            self.put_target(target);
        }

        /// The null selector until one is set, as in Foundation.
        #[unsafe(method(selector))]
        fn selector(&self) -> Option<Sel> {
            self.current_selector()
        }

        #[unsafe(method(setSelector:))]
        fn set_selector(&self, sel: Sel) {
            let slot = self.slot(1, "setArgument:atIndex:");
            // SAFETY: argument 1 is a selector.
            unsafe { self.set(slot, (&raw const sel).cast()) };
        }

        #[unsafe(method(getReturnValue:))]
        fn get_return_value(&self, location: NonNull<c_void>) {
            // SAFETY: the caller passes room for the return value.
            unsafe { self.get(Slot::Return, location.as_ptr().cast()) };
        }

        #[unsafe(method(setReturnValue:))]
        fn set_return_value(&self, location: NonNull<c_void>) {
            // SAFETY: the caller passes a value of the return type.
            unsafe { self.set(Slot::Return, location.as_ptr().cast()) };
        }

        #[unsafe(method(getArgument:atIndex:))]
        fn get_argument(&self, location: NonNull<c_void>, index: NSInteger) {
            let slot = self.slot(index, "getArgument:atIndex:");
            // SAFETY: the caller passes room for the argument.
            unsafe { self.get(slot, location.as_ptr().cast()) };
        }

        #[unsafe(method(setArgument:atIndex:))]
        fn set_argument(&self, location: NonNull<c_void>, index: NSInteger) {
            let slot = self.slot(index, "setArgument:atIndex:");
            // SAFETY: the caller passes a value of the argument's type.
            unsafe { self.set(slot, location.as_ptr().cast()) };
        }

        #[unsafe(method(invoke))]
        fn invoke(&self) {
            self.send();
        }

        #[unsafe(method(invokeWithTarget:))]
        fn invoke_with_target(&self, target: &AnyObject) {
            self.put_target(Some(target));
            self.send();
        }

        #[unsafe(method(invokeUsingIMP:))]
        fn invoke_using_imp(&self, imp: Option<Imp>) {
            self.call(imp);
        }
    }
);

// The target and selector are arguments 0 and 1, read and written as
// `-getArgument:atIndex:` and `-setArgument:atIndex:` would, raising as
// they do for a signature too short to have them.
impl NSInvocationImpl {
    fn put_target(&self, target: Option<&AnyObject>) {
        let slot = self.slot(0, "setArgument:atIndex:");
        let target: *const AnyObject = target.map_or(std::ptr::null(), |t| t);
        // SAFETY: argument 0 is an object.
        unsafe { self.set(slot, (&raw const target).cast()) };
    }

    fn current_selector(&self) -> Option<Sel> {
        let sel = self.pointer(self.slot(1, "getArgument:atIndex:"));
        // SAFETY: argument 1 holds a selector or zero, which `Option<Sel>`,
        // a nullable pointer, represents as `None`.
        unsafe { std::mem::transmute::<*mut AnyObject, Option<Sel>>(sel) }
    }

    /// The pointer in `slot`: the target (0) or the selector (1). Null if
    /// the signature has no pointer there.
    fn pointer(&self, slot: Slot) -> *mut AnyObject {
        let fits = slot.value(&self.ivars().layout).size() == size_of::<*mut AnyObject>();
        // SAFETY: a pointer-sized value.
        if fits { unsafe { read_pointer(self.at(slot)) } } else { std::ptr::null_mut() }
    }

    /// `-invoke`: the message to the target, if there is one.
    fn send(&self) {
        let target = self.pointer(self.slot(0, "getArgument:atIndex:"));
        if target.is_null() {
            return;
        }
        let Some(sel) = self.current_selector() else { return };
        // SAFETY: a live target; the lookup handles unknown selectors.
        let imp = unsafe { ffi::objc_msg_lookup(target, sel) };
        self.call(imp);
    }

    /// Where `slot`'s value lives.
    fn at(&self, slot: Slot) -> *mut u8 {
        let ivars = self.ivars();
        // SAFETY: the buffer holds every value at its offset.
        unsafe { ivars.values.ptr().add(slot.value(&ivars.layout).offset()) }
    }

    /// The slot for `index` (-1 is the return value), or raises as
    /// Foundation does.
    fn slot(&self, index: NSInteger, method: &str) -> Slot {
        let count = self.ivars().layout.arguments().len() as NSInteger;
        match index {
            -1 => Slot::Return,
            i if (0..count).contains(&i) => Slot::Argument(i as usize),
            _ => panic!("-[NSInvocation {method}]: index ({index}) out of bounds [-1, {}]", count - 1),
        }
    }

    /// Copy `slot`'s value to `dst`.
    ///
    /// # Safety
    /// `dst` must have room for the value.
    unsafe fn get(&self, slot: Slot, dst: *mut u8) {
        let size = slot.value(&self.ivars().layout).size();
        // SAFETY: guaranteed by the caller.
        unsafe { dst.copy_from_nonoverlapping(self.at(slot), size) };
    }

    /// Copy `src` into `slot`, retaining (or copying) it if arguments are
    /// retained.
    ///
    /// # Safety
    /// `src` must hold a value of the slot's type.
    unsafe fn set(&self, slot: Slot, src: *const u8) {
        let size = slot.value(&self.ivars().layout).size();
        // SAFETY: guaranteed by the caller.
        unsafe { self.at(slot).copy_from(src, size) };
        if self.ivars().retained.get() {
            // SAFETY: the value just set.
            unsafe { self.retain_in_place(slot) };
        }
    }

    /// Retain (or copy) the value in `slot`, for a retained invocation,
    /// which keeps it until it goes.
    ///
    /// # Safety
    /// The slot must hold a valid value.
    unsafe fn retain_in_place(&self, slot: Slot) {
        let ivars = self.ivars();
        let at = self.at(slot);
        // SAFETY: a pointer-sized slot for these kinds, holding a valid
        // value. Retaining and copying run code of the value's (a copy
        // helper, say), so nothing is borrowed meanwhile.
        unsafe {
            match slot.value(&ivars.layout).kind() {
                ValueKind::Object => {
                    if let Some(object) = NonNull::new(ffi::objc_retain(read_pointer(at))) {
                        ivars.kept.borrow_mut().push(object);
                    }
                }
                ValueKind::Block => {
                    let copy = ffi::objc_retainBlock(read_pointer(at));
                    at.cast::<*mut AnyObject>().write(copy);
                    if let Some(copy) = NonNull::new(copy) {
                        ivars.kept.borrow_mut().push(copy);
                    }
                }
                ValueKind::CString => {
                    let text = at.cast::<*const c_char>().read();
                    if !text.is_null() {
                        let copy = CStr::from_ptr(text).to_owned();
                        // The copy's bytes stay where they are as the
                        // list grows.
                        at.cast::<*const c_char>().write(copy.as_ptr());
                        ivars.strings.borrow_mut().push(copy);
                    }
                }
                ValueKind::Plain => {}
            }
        }
    }

    /// Call `imp` with the arguments, storing the return value.
    fn call(&self, imp: Option<Imp>) {
        let Some(imp) = imp else { return };
        let ivars = self.ivars();
        // SAFETY: the buffer holds the arguments the signature describes,
        // and the implementation is the target's for the selector, which
        // the signature was made for.
        unsafe { ivars.layout.call(imp, ivars.values.ptr()) };
        if ivars.retained.get() {
            // SAFETY: the value the call returned.
            unsafe { self.retain_in_place(Slot::Return) };
        }
    }
}

/// A new invocation for `signature`, with every argument zero.
fn invocation_with_signature(signature: &NSMethodSignature) -> Retained<NSInvocationImpl> {
    let layout = if inherits(signature, &NSMETHODSIGNATURE) {
        // SAFETY: an NSMethodSignature has this class's storage.
        unsafe { &*(signature as *const NSMethodSignature).cast::<NSMethodSignatureImpl>() }.ivars().layout.clone()
    } else {
        // Someone else's subclass: its types, read back.
        let mut types = text_of(signature.methodReturnType()).to_owned();
        for i in 0..signature.numberOfArguments() {
            types.extend_from_slice(text_of(signature.getArgumentTypeAtIndex(i)));
        }
        let Ok(Some(layout)) = Signature::parse(&types) else {
            panic!("*** +[NSInvocation invocationWithMethodSignature:]: unusable method signature");
        };
        Arc::new(layout)
    };
    let values = Buffer::new(layout.values_size());
    let ivars = InvocationIvars {
        signature: signature.retain(),
        layout,
        values,
        retained: Cell::new(false),
        kept: RefCell::default(),
        strings: RefCell::default(),
    };
    // Sending +alloc through the binding loads the class on first use.
    let this = NSInvocation::alloc();
    // SAFETY: NSInvocation's class is NSInvocationImpl, and an
    // `Allocated` is a pointer to its object whatever its type parameter.
    let this = unsafe { std::mem::transmute::<Allocated<NSInvocation>, Allocated<NSInvocationImpl>>(this) };
    let this = this.set_ivars(ivars);
    // SAFETY: NSObject's initializer.
    unsafe { msg_send![super(this), init] }
}

fn text_of<'a>(p: NonNull<c_char>) -> &'a [u8] {
    // SAFETY: the bindings hand out C strings that live as long as their
    // signature.
    unsafe { CStr::from_ptr(p.as_ptr()) }.to_bytes()
}
