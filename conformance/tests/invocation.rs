//! `NSMethodSignature`, `NSInvocation`, and forwarding through
//! `-methodSignatureForSelector:` and `-forwardInvocation:`.
//!
//! Invocations call methods of every shape the C calling conventions
//! treat differently: integers and floating-point values past the
//! registers, narrow integers, small structs of floats (passed in
//! floating-point registers), small mixed structs, and structs large
//! enough to go through memory, as arguments and as return values. The
//! same methods are then sent to an object that forwards them, so the
//! arguments are read from a real call and the return value written back
//! into it.
//!
//! objc2 checks in debug builds that a message's method exists before
//! sending it, so forwarded messages are sent through the implementation
//! `-methodForSelector:` hands out, which forwards however it is called.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_char};
use std::mem::MaybeUninit;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, Imp, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Encode, Encoding, Message, define_class, msg_send, sel};
use objc2_foundation::{NSInvocation, NSMethodSignature, NSPoint, NSRange, NSRect, NSSize};

use sidestep as _;

/// Eight bytes with padding inside: one integer register everywhere.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Mixed {
    c: i8,
    i: i32,
}

unsafe impl Encode for Mixed {
    const ENCODING: Encoding = Encoding::Struct("SidestepMixed", &[i8::ENCODING, i32::ENCODING]);
}

/// Three floats: floating-point registers, one per field on aarch64 and
/// two to a register on x86_64.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Float3 {
    a: f32,
    b: f32,
    c: f32,
}

unsafe impl Encode for Float3 {
    const ENCODING: Encoding = Encoding::Struct("SidestepFloat3", &[f32::ENCODING; 3]);
}

/// A double and an integer: two integer registers on aarch64, one of each
/// kind on x86_64.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct DoubleLong {
    d: f64,
    q: i64,
}

unsafe impl Encode for DoubleLong {
    const ENCODING: Encoding = Encoding::Struct("SidestepDoubleLong", &[f64::ENCODING, i64::ENCODING]);
}

/// A float and an integer sharing eight bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FloatInt {
    f: f32,
    i: i32,
}

unsafe impl Encode for FloatInt {
    const ENCODING: Encoding = Encoding::Struct("SidestepFloatInt", &[f32::ENCODING, i32::ENCODING]);
}

/// Twelve bytes: two integer registers.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Int3 {
    a: i32,
    b: i32,
    c: i32,
}

unsafe impl Encode for Int3 {
    const ENCODING: Encoding = Encoding::Struct("SidestepInt3", &[i32::ENCODING; 3]);
}

/// Three bytes, less than a register.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Bytes3 {
    a: u8,
    b: u8,
    c: u8,
}

unsafe impl Encode for Bytes3 {
    const ENCODING: Encoding = Encoding::Struct("SidestepBytes3", &[u8::ENCODING; 3]);
}

/// Forty bytes of integers: through memory on every architecture.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Wide {
    v: [i64; 5],
}

unsafe impl Encode for Wide {
    const ENCODING: Encoding = Encoding::Struct("SidestepWide", &[<[i64; 5]>::ENCODING]);
}

/// Five doubles: too many for the floating-point registers, so through
/// memory too.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Doubles5 {
    v: [f64; 5],
}

unsafe impl Encode for Doubles5 {
    const ENCODING: Encoding = Encoding::Struct("SidestepDoubles5", &[<[f64; 5]>::ENCODING]);
}

define_class!(
    /// Counts its deallocations in a counter of its own test's, to see
    /// who keeps it alive.
    #[unsafe(super(NSObject))]
    #[name = "SidestepInvTracked"]
    #[ivars = &'static AtomicUsize]
    struct Tracked;
);

impl Drop for Tracked {
    fn drop(&mut self) {
        self.ivars().fetch_add(1, Ordering::SeqCst);
    }
}

/// Made through objc2's initialization, which is what runs `Drop`.
fn tracked(deallocs: &'static AtomicUsize) -> Retained<Tracked> {
    let this = Tracked::alloc().set_ivars(deallocs);
    unsafe { msg_send![super(this), init] }
}

struct CalleeIvars {
    base: i64,
    calls: Cell<usize>,
}

define_class!(
    /// The methods invocations call.
    #[unsafe(super(NSObject))]
    #[name = "SidestepInvCallee"]
    #[ivars = CalleeIvars]
    struct Callee;

    impl Callee {
        #[unsafe(method(ints:b:c:d:e:f:g:h:i:j:))]
        #[allow(clippy::too_many_arguments)]
        fn ints(&self, a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64, i: i64, j: i64) -> i64 {
            self.ivars().calls.set(self.ivars().calls.get() + 1);
            // Weighted, so arguments arriving in the wrong places show.
            [a, b, c, d, e, f, g, h, i, j].iter().enumerate().map(|(n, v)| (n as i64 + 1) * v).sum::<i64>()
                + self.ivars().base
        }

        #[unsafe(method(doubles:b:c:d:e:f:g:h:i:j:))]
        #[allow(clippy::too_many_arguments)]
        fn doubles(&self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, i: f64, j: f64) -> f64 {
            [a, b, c, d, e, f, g, h, i, j].iter().enumerate().map(|(n, v)| (n as f64 + 1.0) * v).sum::<f64>()
                + self.ivars().base as f64
        }

        /// Every narrow type, and floats beside doubles.
        #[unsafe(method(narrow:s:u:w:i:f:d:q:))]
        #[allow(clippy::too_many_arguments)]
        fn narrow(&self, c: i8, s: i16, u: u8, w: u16, i: i32, f: f32, d: f64, q: u64) -> f64 {
            c as f64 * 1e6 + s as f64 * 1e3 + u as f64 + w as f64 * 1e-3 + i as f64 * 1e9 + f as f64 * 0.5
                + d * 0.25
                + q as f64
        }

        #[unsafe(method(negativeChar))]
        fn negative_char(&self) -> i8 {
            -5
        }

        #[unsafe(method(bigShort))]
        fn big_short(&self) -> u16 {
            65000
        }

        #[unsafe(method(floatOf:))]
        fn float_of(&self, x: f32) -> f32 {
            x * 2.0
        }

        #[unsafe(method(isPositive:))]
        fn is_positive(&self, x: i64) -> bool {
            x > 0
        }

        #[unsafe(method(point:))]
        fn point(&self, p: NSPoint) -> NSPoint {
            NSPoint::new(p.x * 2.0, p.y + self.ivars().base as f64)
        }

        #[unsafe(method(rect:))]
        fn rect(&self, r: NSRect) -> NSRect {
            NSRect::new(NSPoint::new(r.origin.y, r.origin.x), NSSize::new(r.size.width * 2.0, r.size.height * 3.0))
        }

        #[unsafe(method(range:))]
        fn range(&self, r: NSRange) -> NSRange {
            NSRange::new(r.location + 1, r.length * 2)
        }

        #[unsafe(method(mixed:))]
        fn mixed(&self, m: Mixed) -> Mixed {
            Mixed { c: m.c - 1, i: m.i * 2 }
        }

        #[unsafe(method(float3:))]
        fn float3(&self, v: Float3) -> Float3 {
            Float3 { a: v.c, b: v.a, c: v.b + 0.5 }
        }

        #[unsafe(method(doubleLong:))]
        fn double_long(&self, v: DoubleLong) -> DoubleLong {
            DoubleLong { d: v.d * 2.0, q: v.q - 1 }
        }

        #[unsafe(method(floatInt:))]
        fn float_int(&self, v: FloatInt) -> FloatInt {
            FloatInt { f: v.f + 1.0, i: -v.i }
        }

        #[unsafe(method(int3:))]
        fn int3(&self, v: Int3) -> Int3 {
            Int3 { a: v.c, b: v.b * 10, c: v.a }
        }

        #[unsafe(method(bytes3:))]
        fn bytes3(&self, v: Bytes3) -> Bytes3 {
            Bytes3 { a: v.c, b: v.a, c: v.b.wrapping_add(1) }
        }

        #[unsafe(method(wide:))]
        fn wide(&self, v: Wide) -> Wide {
            let mut out = v.v;
            out.reverse();
            out[0] += self.ivars().base;
            Wide { v: out }
        }

        #[unsafe(method(doubles5:))]
        fn doubles5(&self, v: Doubles5) -> Doubles5 {
            Doubles5 { v: v.v.map(|x| x * 2.0) }
        }

        /// A struct too big for the integer registers left, and an integer
        /// after it.
        #[unsafe(method(a:b:c:range:d:))]
        fn spill_range_early(&self, a: i64, b: i64, c: i64, r: NSRange, d: i64) -> i64 {
            a + 10 * b + 100 * c + 1000 * r.location as i64 + 10000 * r.length as i64 + 100000 * d
        }

        #[unsafe(method(a:b:c:d:e:f:range:g:))]
        #[allow(clippy::too_many_arguments)]
        fn spill_range_late(&self, a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, r: NSRange, g: i64) -> i64 {
            a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * r.location as i64 + 8 * r.length as i64 + 9 * g
        }

        /// A struct of doubles after the floating-point registers are
        /// nearly used up, and a double after it.
        #[unsafe(method(d:d:d:d:d:d:d:point:d:))]
        #[allow(clippy::too_many_arguments)]
        fn spill_point(&self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, p: NSPoint, h: f64) -> f64 {
            a + 2.0 * b + 3.0 * c + 4.0 * d + 5.0 * e + 6.0 * f + 7.0 * g + 8.0 * p.x + 9.0 * p.y + 10.0 * h
        }

        #[unsafe(method(d:d:d:d:d:d:d:d:rect:))]
        #[allow(clippy::too_many_arguments)]
        fn spill_rect(&self, a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, r: NSRect) -> NSRect {
            let sum = a + b + c + d + e + f + g + h;
            NSRect::new(NSPoint::new(r.origin.x + sum, r.origin.y), r.size)
        }

        #[unsafe(method(wide:after:))]
        fn wide_after(&self, v: Wide, x: f64) -> f64 {
            v.v.iter().sum::<i64>() as f64 + x
        }

        #[unsafe(method(first:second:))]
        fn first_second(&self, a: &AnyObject, b: Option<&AnyObject>) -> *mut AnyObject {
            match b {
                Some(b) => (b as *const AnyObject).cast_mut(),
                None => (a as *const AnyObject).cast_mut(),
            }
        }

        #[unsafe(method(echoSelector:))]
        fn echo_selector(&self, sel: Sel) -> Sel {
            sel
        }

        #[unsafe(method(store:into:))]
        fn store(&self, value: i64, into: *mut i64) {
            unsafe { *into = value + self.ivars().base };
        }

        #[unsafe(method(lengthOf:))]
        fn length_of(&self, s: *const c_char) -> usize {
            unsafe { CStr::from_ptr(s) }.to_bytes().len()
        }

        #[unsafe(method(classValue:))]
        fn class_value(x: i64) -> i64 {
            x * 7
        }
    }
);

impl Callee {
    fn new(base: i64) -> Retained<Self> {
        let this = Self::alloc().set_ivars(CalleeIvars { base, calls: Cell::new(0) });
        unsafe { msg_send![super(this), init] }
    }
}

fn signature(types: &CStr) -> Option<Retained<NSMethodSignature>> {
    unsafe { NSMethodSignature::signatureWithObjCTypes(NonNull::new(types.as_ptr().cast_mut()).unwrap()) }
}

fn text(p: NonNull<c_char>) -> String {
    unsafe { CStr::from_ptr(p.as_ptr()) }.to_str().unwrap().to_owned()
}

fn arg_types(sig: &NSMethodSignature) -> Vec<String> {
    (0..sig.numberOfArguments()).map(|i| text(sig.getArgumentTypeAtIndex(i))).collect()
}

/// The reason an operation fails with: Apple raises an NSException;
/// Sidestep panics with the same message.
fn failure(f: impl FnOnce()) -> String {
    #[cfg(target_vendor = "apple")]
    {
        match objc2::exception::catch(AssertUnwindSafe(f)) {
            Ok(()) => panic!("expected an exception"),
            Err(Some(e)) => {
                let reason: Retained<objc2_foundation::NSString> = unsafe { msg_send![&*e, reason] };
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

#[test]
fn signatures_describe_their_types() {
    let sig = signature(c"v@:").unwrap();
    assert_eq!(sig.numberOfArguments(), 2);
    assert_eq!(text(sig.methodReturnType()), "v");
    assert_eq!(sig.methodReturnLength(), 0);
    assert_eq!(arg_types(&sig), ["@", ":"]);
    assert!(!sig.isOneway());
    assert!(sig.frameLength() >= 16);

    // Offsets are dropped; qualifiers, class names and block signatures
    // are kept.
    let sig = signature(c"i24@0:8i16").unwrap();
    assert_eq!((text(sig.methodReturnType()), sig.methodReturnLength()), ("i".into(), 4));
    assert_eq!(arg_types(&sig), ["@", ":", "i"]);
    let sig = signature(c"v@:r*@\"NSString\"@?<v@?>[4i]^{Foo=i}{?=ii}Q16").unwrap();
    assert_eq!(arg_types(&sig), ["@", ":", "r*", "@\"NSString\"", "@?<v@?>", "[4i]", "^{Foo=i}", "{?=ii}", "Q"]);
    assert!(signature(c"Vv@:").unwrap().isOneway());

    for (types, len) in [
        (c"c@:", 1),
        (c"B@:", 1),
        (c"s@:", 2),
        (c"l@:", 4),
        (c"q@:", 8),
        (c"f@:", 4),
        (c"d@:", 8),
        (c"*@:", 8),
        (c"#@:", 8),
        (c":@:", 8),
        (c"^v@:", 8),
        (c"{CGPoint=dd}@:", 16),
        (c"{CGRect={CGPoint=dd}{CGSize=dd}}@:", 32),
        (c"{SidestepWide=[5q]}@:", 40),
        (c"{SidestepMixed=ci}@:", 8),
        (c"{SidestepBytes3=CCC}@:", 3),
    ] {
        let sig = signature(types).unwrap();
        assert_eq!(sig.methodReturnLength(), len, "{types:?}");
        assert_eq!(text(sig.methodReturnType()), types.to_str().unwrap().trim_end_matches("@:"));
    }

    // A return type alone is a signature with no arguments.
    let sig = signature(c"@").unwrap();
    assert_eq!((sig.numberOfArguments(), sig.methodReturnLength()), (0, 8));
    assert!(signature(c"").is_none());
}

#[test]
fn signatures_refuse_what_they_cannot_lay_out() {
    for (types, spec) in [(c"x@:", "'x'"), (c"v@:b3", "'b'"), (c"v@:(U=id)", "'('")] {
        let reason = failure(|| {
            let _ = signature(types);
        });
        assert!(reason.contains(&format!("unsupported type encoding spec {spec}")), "{reason}");
    }
}

/// `-methodSignatureForSelector:` describes the receiver's own methods,
/// class methods when the receiver is a class, and nothing else.
#[test]
fn objects_describe_their_methods() {
    let callee = Callee::new(0);
    let sig: Option<Retained<NSMethodSignature>> =
        unsafe { msg_send![&*callee, methodSignatureForSelector: sel!(point:)] };
    let sig = sig.expect("a signature");
    assert_eq!(sig.numberOfArguments(), 3);
    assert_eq!(sig.methodReturnLength(), size_of::<NSPoint>());
    let sig: Option<Retained<NSMethodSignature>> =
        unsafe { msg_send![&*callee, methodSignatureForSelector: sel!(sidestepNoSuchMethod)] };
    assert!(sig.is_none());
    let sig: Option<Retained<NSMethodSignature>> =
        unsafe { msg_send![Callee::class(), methodSignatureForSelector: sel!(classValue:)] };
    assert_eq!(sig.expect("a class method's signature").numberOfArguments(), 3);
    let sig: Option<Retained<NSMethodSignature>> =
        unsafe { msg_send![Callee::class(), instanceMethodSignatureForSelector: sel!(range:)] };
    assert_eq!(sig.expect("an instance method's signature").methodReturnLength(), size_of::<NSRange>());
    let sig: Option<Retained<NSMethodSignature>> =
        unsafe { msg_send![Callee::class(), instanceMethodSignatureForSelector: sel!(classValue:)] };
    assert!(sig.is_none());
}

/// An invocation for `sel` on `target`, from the target's own signature.
fn invocation(target: &AnyObject, sel: Sel) -> Retained<NSInvocation> {
    let sig: Option<Retained<NSMethodSignature>> = unsafe { msg_send![target, methodSignatureForSelector: sel] };
    let inv = unsafe { NSInvocation::invocationWithMethodSignature(&sig.expect("a signature")) };
    unsafe {
        inv.setTarget(Some(target));
        inv.setSelector(sel);
    }
    inv
}

fn set<T>(inv: &NSInvocation, index: isize, value: &T) {
    unsafe { inv.setArgument_atIndex(NonNull::from(value).cast(), index) };
}

fn get<T: Copy>(inv: &NSInvocation, index: isize) -> T {
    let mut value = MaybeUninit::<T>::zeroed();
    unsafe {
        inv.getArgument_atIndex(NonNull::new(value.as_mut_ptr()).unwrap().cast(), index);
        value.assume_init()
    }
}

fn returned<T: Copy>(inv: &NSInvocation) -> T {
    let mut value = MaybeUninit::<T>::zeroed();
    unsafe {
        inv.getReturnValue(NonNull::new(value.as_mut_ptr()).unwrap().cast());
        value.assume_init()
    }
}

/// Invoke `sel` on `target` with `args` (pointers to each argument) and
/// read back an `R`.
fn call<R: Copy>(target: &AnyObject, sel: Sel, args: &[&dyn Arg]) -> R {
    let inv = invocation(target, sel);
    for (i, arg) in args.iter().enumerate() {
        unsafe { inv.setArgument_atIndex((**arg).ptr(), i as isize + 2) };
    }
    unsafe { inv.invoke() };
    returned(&inv)
}

/// Something an argument can be copied from.
trait Arg {
    fn ptr(&self) -> NonNull<std::ffi::c_void>;
}

impl<T> Arg for T {
    fn ptr(&self) -> NonNull<std::ffi::c_void> {
        NonNull::from(self).cast()
    }
}

#[test]
fn new_invocations_are_empty() {
    let inv = unsafe { NSInvocation::invocationWithMethodSignature(&signature(c"q@:q").unwrap()) };
    assert!(unsafe { inv.target() }.is_none());
    assert!(!unsafe { inv.argumentsRetained() });
    assert_eq!(get::<i64>(&inv, 2), 0);
    assert_eq!(returned::<i64>(&inv), 0);
    assert_eq!(unsafe { inv.methodSignature() }.numberOfArguments(), 3);
}

#[test]
fn arguments_read_back() {
    let callee = Callee::new(0);
    let inv = invocation(&callee, sel!(rect:));
    let rect = NSRect::new(NSPoint::new(1.0, 2.0), NSSize::new(3.0, 4.0));
    set(&inv, 2, &rect);
    assert_eq!(get::<NSRect>(&inv, 2), rect);
    // Index 0 is the target and 1 the selector; -1 is the return value.
    assert_eq!(get::<*const AnyObject>(&inv, 0), Retained::as_ptr(&callee).cast());
    assert_eq!(get::<Sel>(&inv, 1), sel!(rect:));
    assert_eq!(unsafe { inv.selector() }, sel!(rect:));
    set(&inv, -1, &rect);
    assert_eq!(returned::<NSRect>(&inv), rect);
    let other = NSObject::new();
    set(&inv, 0, &Retained::as_ptr(&other));
    assert_eq!(
        unsafe { inv.target() }.as_deref().map(|t| t as *const AnyObject),
        Some(Retained::as_ptr(&other).cast())
    );
    set(&inv, 1, &sel!(point:));
    assert_eq!(unsafe { inv.selector() }, sel!(point:));

    for index in [3, -2] {
        let reason = failure(|| {
            let _: NSRect = get(&inv, index);
        });
        assert_eq!(reason, format!("-[NSInvocation getArgument:atIndex:]: index ({index}) out of bounds [-1, 2]"));
        let reason = failure(|| set(&inv, index, &rect));
        assert_eq!(reason, format!("-[NSInvocation setArgument:atIndex:]: index ({index}) out of bounds [-1, 2]"));
    }
}

#[test]
fn invokes_integer_and_floating_point_arguments() {
    let callee = Callee::new(100);
    let args: Vec<i64> = (1..=10).collect();
    let dyn_args: Vec<&dyn Arg> = args.iter().map(|a| a as &dyn Arg).collect();
    let got: i64 = call(&callee, sel!(ints:b:c:d:e:f:g:h:i:j:), &dyn_args);
    assert_eq!(got, (1..=10).map(|n| n * n).sum::<i64>() + 100);
    let args: Vec<f64> = (1..=10).map(|n| n as f64 / 2.0).collect();
    let dyn_args: Vec<&dyn Arg> = args.iter().map(|a| a as &dyn Arg).collect();
    let got: f64 = call(&callee, sel!(doubles:b:c:d:e:f:g:h:i:j:), &dyn_args);
    assert_eq!(got, (1..=10).map(|n| (n * n) as f64 / 2.0).sum::<f64>() + 100.0);
    // Narrow signed values are positive here: Apple's NSInvocation passes
    // them without the sign extension its own calling convention asks of
    // callers (forwarded messages below check negative ones).
    let got: f64 =
        call(&callee, sel!(narrow:s:u:w:i:f:d:q:), &[&3i8, &20i16, &200u8, &60000u16, &-2i32, &3.0f32, &8.0f64, &7u64]);
    assert_eq!(got, 3e6 + 20e3 + 200.0 + 60.0 - 2e9 + 1.5 + 2.0 + 7.0);
    assert_eq!(call::<i8>(&callee, sel!(negativeChar), &[]), -5);
    assert_eq!(call::<u16>(&callee, sel!(bigShort), &[]), 65000);
    assert_eq!(call::<f32>(&callee, sel!(floatOf:), &[&1.25f32]), 2.5);
    assert!(call::<bool>(&callee, sel!(isPositive:), &[&3i64]));
    assert!(!call::<bool>(&callee, sel!(isPositive:), &[&-3i64]));
    assert_eq!(callee.ivars().calls.get(), 1);
}

#[test]
fn invokes_struct_arguments_and_returns() {
    let callee = Callee::new(100);
    assert_eq!(call::<NSPoint>(&callee, sel!(point:), &[&NSPoint::new(1.5, 2.0)]), NSPoint::new(3.0, 102.0));
    let rect = NSRect::new(NSPoint::new(1.0, 2.0), NSSize::new(3.0, 4.0));
    assert_eq!(
        call::<NSRect>(&callee, sel!(rect:), &[&rect]),
        NSRect::new(NSPoint::new(2.0, 1.0), NSSize::new(6.0, 12.0))
    );
    assert_eq!(call::<NSRange>(&callee, sel!(range:), &[&NSRange::new(5, 6)]), NSRange::new(6, 12));
    assert_eq!(call::<Mixed>(&callee, sel!(mixed:), &[&Mixed { c: -1, i: 21 }]), Mixed { c: -2, i: 42 });
    assert_eq!(
        call::<Float3>(&callee, sel!(float3:), &[&Float3 { a: 1.0, b: 2.0, c: 3.0 }]),
        Float3 { a: 3.0, b: 1.0, c: 2.5 }
    );
    assert_eq!(
        call::<DoubleLong>(&callee, sel!(doubleLong:), &[&DoubleLong { d: 1.25, q: 10 }]),
        DoubleLong { d: 2.5, q: 9 }
    );
    assert_eq!(call::<FloatInt>(&callee, sel!(floatInt:), &[&FloatInt { f: 1.5, i: 4 }]), FloatInt { f: 2.5, i: -4 });
    assert_eq!(call::<Int3>(&callee, sel!(int3:), &[&Int3 { a: 1, b: 2, c: 3 }]), Int3 { a: 3, b: 20, c: 1 });
    assert_eq!(
        call::<Bytes3>(&callee, sel!(bytes3:), &[&Bytes3 { a: 1, b: 2, c: 255 }]),
        Bytes3 { a: 255, b: 1, c: 3 }
    );
    assert_eq!(call::<Wide>(&callee, sel!(wide:), &[&Wide { v: [1, 2, 3, 4, 5] }]), Wide { v: [105, 4, 3, 2, 1] });
    assert_eq!(
        call::<Doubles5>(&callee, sel!(doubles5:), &[&Doubles5 { v: [1.0, 2.0, 3.0, 4.0, 5.0] }]),
        Doubles5 { v: [2.0, 4.0, 6.0, 8.0, 10.0] }
    );
}

#[test]
fn invokes_structs_past_the_registers() {
    let callee = Callee::new(0);
    let got: i64 = call(&callee, sel!(a:b:c:range:d:), &[&1i64, &2i64, &3i64, &NSRange::new(4, 5), &6i64]);
    assert_eq!(got, 654321);
    let got: i64 = call(
        &callee,
        sel!(a:b:c:d:e:f:range:g:),
        &[&1i64, &2i64, &3i64, &4i64, &5i64, &6i64, &NSRange::new(7, 8), &9i64],
    );
    assert_eq!(got, (1..=9).map(|n| n * n).sum::<i64>());
    let got: f64 = call(
        &callee,
        sel!(d:d:d:d:d:d:d:point:d:),
        &[&1.0f64, &2.0f64, &3.0f64, &4.0f64, &5.0f64, &6.0f64, &7.0f64, &NSPoint::new(8.0, 9.0), &10.0f64],
    );
    assert_eq!(got, (1..=10).map(|n| (n * n) as f64).sum::<f64>());
    let rect = NSRect::new(NSPoint::new(0.5, 2.0), NSSize::new(3.0, 4.0));
    let got: NSRect = call(
        &callee,
        sel!(d:d:d:d:d:d:d:d:rect:),
        &[&1.0f64, &2.0f64, &3.0f64, &4.0f64, &5.0f64, &6.0f64, &7.0f64, &8.0f64, &rect],
    );
    assert_eq!(got, NSRect::new(NSPoint::new(36.5, 2.0), NSSize::new(3.0, 4.0)));
    let got: f64 = call(&callee, sel!(wide:after:), &[&Wide { v: [1, 2, 3, 4, 5] }, &0.5f64]);
    assert_eq!(got, 15.5);
}

#[test]
fn invokes_objects_selectors_and_pointers() {
    let callee = Callee::new(100);
    let a = NSObject::new();
    let b = NSObject::new();
    let (pa, pb) = (Retained::as_ptr(&a), Retained::as_ptr(&b));
    assert_eq!(call::<*const NSObject>(&callee, sel!(first:second:), &[&pa, &pb]), pb);
    assert_eq!(call::<*const NSObject>(&callee, sel!(first:second:), &[&pa, &std::ptr::null::<NSObject>()]), pa);
    assert_eq!(call::<Sel>(&callee, sel!(echoSelector:), &[&sel!(hash)]), sel!(hash));
    let mut out = 0i64;
    let into: *mut i64 = &mut out;
    let inv = invocation(&callee, sel!(store:into:));
    set(&inv, 2, &5i64);
    set(&inv, 3, &into);
    unsafe { inv.invoke() };
    assert_eq!(out, 105);
    let s: *const c_char = c"four".as_ptr();
    assert_eq!(call::<usize>(&callee, sel!(lengthOf:), &[&s]), 4);
}

#[test]
fn invokes_class_methods_and_other_targets() {
    let cls: &AnyObject = unsafe { &*(Callee::class() as *const AnyClass).cast::<AnyObject>() };
    assert_eq!(call::<i64>(cls, sel!(classValue:), &[&6i64]), 42);

    let first = Callee::new(1);
    let second = Callee::new(2);
    let inv = invocation(&first, sel!(point:));
    set(&inv, 2, &NSPoint::new(1.0, 1.0));
    unsafe { inv.invokeWithTarget(&second) };
    assert_eq!(returned::<NSPoint>(&inv), NSPoint::new(2.0, 3.0));
    // The target is now the one it was invoked with.
    assert_eq!(
        unsafe { inv.target() }.as_deref().map(|t| t as *const AnyObject),
        Some(Retained::as_ptr(&second).cast())
    );

    let imp: Option<Imp> = unsafe { msg_send![&*first, methodForSelector: sel!(point:)] };
    unsafe { inv.setTarget(Some(&first)) };
    unsafe { inv.invokeUsingIMP(imp) };
    assert_eq!(returned::<NSPoint>(&inv), NSPoint::new(2.0, 2.0));
}

#[test]
fn a_nil_target_is_not_invoked() {
    let callee = Callee::new(0);
    let inv = invocation(&callee, sel!(isPositive:));
    unsafe { inv.setTarget(None) };
    set(&inv, -1, &true);
    unsafe { inv.invoke() };
    // Nothing ran, so the return value is as it was.
    assert!(returned::<bool>(&inv));
}

/// Invocations come back autoreleased, so these tests run in pools of
/// their own to see when things are freed.
#[test]
fn retained_arguments_outlive_their_owners() {
    static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
    let callee = Callee::new(0);
    let (pa, pb) = autoreleasepool(|_| {
        let inv = invocation(&callee, sel!(first:second:));
        let (a, b) = (tracked(&DEALLOCS), tracked(&DEALLOCS));
        set(&inv, 2, &Retained::as_ptr(&a));
        unsafe { inv.retainArguments() };
        assert!(unsafe { inv.argumentsRetained() });
        // Set once arguments are retained, so retained too.
        set(&inv, 3, &Retained::as_ptr(&b));
        let (pa, pb) = (Retained::as_ptr(&a), Retained::as_ptr(&b));
        drop((a, b));
        assert_eq!(DEALLOCS.load(Ordering::SeqCst), 0);
        unsafe { inv.invoke() };
        assert_eq!(returned::<*const Tracked>(&inv), pb);
        assert_eq!(get::<*const Tracked>(&inv, 2), pa);
        (pa, pb)
    });
    assert_ne!(pa, pb);
    assert_eq!(DEALLOCS.load(Ordering::SeqCst), 2);
}

#[test]
fn retained_strings_are_copied() {
    let callee = Callee::new(0);
    let inv = invocation(&callee, sel!(lengthOf:));
    let mut text = *b"hello\0";
    let s: *const c_char = text.as_ptr().cast();
    set(&inv, 2, &s);
    unsafe { inv.retainArguments() };
    text[1] = 0;
    std::hint::black_box(&text);
    let copy: *const c_char = get(&inv, 2);
    assert_ne!(copy, s);
    assert_eq!(unsafe { CStr::from_ptr(copy) }, c"hello");
    unsafe { inv.invoke() };
    assert_eq!(returned::<usize>(&inv), 5);
}

struct ForwarderIvars {
    target: Retained<Callee>,
    log: RefCell<Vec<&'static str>>,
    /// Answers `sidestepAnswer` itself, without a target.
    answer: i64,
}

define_class!(
    /// Forwards what it doesn't implement to its target through
    /// `-forwardInvocation:`, logging each step.
    #[unsafe(super(NSObject))]
    #[name = "SidestepInvForwarder"]
    #[ivars = ForwarderIvars]
    struct Forwarder;

    impl Forwarder {
        #[unsafe(method(forwardingTargetForSelector:))]
        fn forwarding_target(&self, _sel: Sel) -> *mut AnyObject {
            self.ivars().log.borrow_mut().push("target");
            std::ptr::null_mut()
        }

        #[unsafe(method_id(methodSignatureForSelector:))]
        fn method_signature(&self, sel: Sel) -> Option<Retained<NSMethodSignature>> {
            self.ivars().log.borrow_mut().push("signature");
            if sel == sel!(sidestepAnswer) {
                signature(c"q@:")
            } else if sel == sel!(sidestepUnknownToEveryone) {
                None
            } else {
                unsafe { msg_send![&*self.ivars().target, methodSignatureForSelector: sel] }
            }
        }

        #[unsafe(method(forwardInvocation:))]
        fn forward_invocation(&self, inv: &NSInvocation) {
            self.ivars().log.borrow_mut().push("forward");
            assert_eq!(unsafe { inv.target() }.as_deref().map(|t| t as *const AnyObject), Some((self as *const Self).cast()));
            if unsafe { inv.selector() } == sel!(sidestepAnswer) {
                set(inv, -1, &self.ivars().answer);
                return;
            }
            unsafe { inv.invokeWithTarget(&self.ivars().target) };
        }
    }
);

impl Forwarder {
    fn new(base: i64) -> Retained<Self> {
        let this =
            Self::alloc().set_ivars(ForwarderIvars { target: Callee::new(base), log: RefCell::default(), answer: 77 });
        unsafe { msg_send![super(this), init] }
    }
}

/// The implementation a message to `obj` with `sel` resolves to, as a
/// function of type `F`.
fn imp_for<F: Copy>(obj: &AnyObject, sel: Sel) -> F {
    let imp: Option<Imp> = unsafe { msg_send![obj, methodForSelector: sel] };
    let imp = imp.expect("an implementation");
    assert_eq!(size_of::<F>(), size_of::<Imp>());
    unsafe { std::mem::transmute_copy(&imp) }
}

type Id = *const AnyObject;

#[test]
fn forwarding_asks_the_target_then_the_signature_then_forwards() {
    let fwd = Forwarder::new(100);
    let answer: unsafe extern "C-unwind" fn(Id, Sel) -> i64 = imp_for(&fwd, sel!(sidestepAnswer));
    assert_eq!(unsafe { answer(Retained::as_ptr(&fwd).cast(), sel!(sidestepAnswer)) }, 77);
    assert_eq!(*fwd.ivars().log.borrow(), ["target", "signature", "forward"]);
    fwd.ivars().log.borrow_mut().clear();
    let point: unsafe extern "C-unwind" fn(Id, Sel, NSPoint) -> NSPoint = imp_for(&fwd, sel!(point:));
    assert_eq!(
        unsafe { point(Retained::as_ptr(&fwd).cast(), sel!(point:), NSPoint::new(1.0, 2.0)) },
        NSPoint::new(2.0, 102.0)
    );
    assert_eq!(*fwd.ivars().log.borrow(), ["target", "signature", "forward"]);
    assert_eq!(fwd.ivars().target.ivars().calls.get(), 0);
    // Forwarding doesn't make the class respond to what it forwards.
    assert!(!fwd.respondsToSelector(sel!(point:)));
}

type Ints = unsafe extern "C-unwind" fn(Id, Sel, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64) -> i64;
type Doubles = unsafe extern "C-unwind" fn(Id, Sel, f64, f64, f64, f64, f64, f64, f64, f64, f64, f64) -> f64;
type Narrow = unsafe extern "C-unwind" fn(Id, Sel, i8, i16, u8, u16, i32, f32, f64, u64) -> f64;

#[test]
fn forwards_arguments_of_every_kind() {
    let fwd = Forwarder::new(100);
    let this: Id = Retained::as_ptr(&fwd).cast();
    let sel = sel!(ints:b:c:d:e:f:g:h:i:j:);
    let ints: Ints = imp_for(&fwd, sel);
    assert_eq!(unsafe { ints(this, sel, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10) }, (1..=10).map(|n| n * n).sum::<i64>() + 100);
    let sel = sel!(doubles:b:c:d:e:f:g:h:i:j:);
    let doubles: Doubles = imp_for(&fwd, sel);
    let got = unsafe { doubles(this, sel, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0) };
    assert_eq!(got, (1..=10).map(|n| (n * n) as f64 / 2.0).sum::<f64>() + 100.0);
    let sel = sel!(narrow:s:u:w:i:f:d:q:);
    let narrow: Narrow = imp_for(&fwd, sel);
    let got = unsafe { narrow(this, sel, -3, -20, 200, 60000, -2, 3.0, 8.0, 7) };
    assert_eq!(got, -3e6 - 20e3 + 200.0 + 60.0 - 2e9 + 1.5 + 2.0 + 7.0);

    let sel = sel!(negativeChar);
    let f: unsafe extern "C-unwind" fn(Id, Sel) -> i8 = imp_for(&fwd, sel);
    assert_eq!(unsafe { f(this, sel) }, -5);
    let sel = sel!(bigShort);
    let f: unsafe extern "C-unwind" fn(Id, Sel) -> u16 = imp_for(&fwd, sel);
    assert_eq!(unsafe { f(this, sel) }, 65000);
    let sel = sel!(floatOf:);
    let f: unsafe extern "C-unwind" fn(Id, Sel, f32) -> f32 = imp_for(&fwd, sel);
    assert_eq!(unsafe { f(this, sel, 1.25) }, 2.5);
    let sel = sel!(isPositive:);
    let f: unsafe extern "C-unwind" fn(Id, Sel, i64) -> bool = imp_for(&fwd, sel);
    assert!(unsafe { f(this, sel, 3) });
    assert!(!unsafe { f(this, sel, -3) });

    let sel = sel!(a:b:c:range:d:);
    let f: unsafe extern "C-unwind" fn(Id, Sel, i64, i64, i64, NSRange, i64) -> i64 = imp_for(&fwd, sel);
    assert_eq!(unsafe { f(this, sel, 1, 2, 3, NSRange::new(4, 5), 6) }, 654321);
    let sel = sel!(a:b:c:d:e:f:range:g:);
    type Late = unsafe extern "C-unwind" fn(Id, Sel, i64, i64, i64, i64, i64, i64, NSRange, i64) -> i64;
    let f: Late = imp_for(&fwd, sel);
    assert_eq!(unsafe { f(this, sel, 1, 2, 3, 4, 5, 6, NSRange::new(7, 8), 9) }, (1..=9).map(|n| n * n).sum::<i64>());
    let sel = sel!(d:d:d:d:d:d:d:point:d:);
    type SpillPoint = unsafe extern "C-unwind" fn(Id, Sel, f64, f64, f64, f64, f64, f64, f64, NSPoint, f64) -> f64;
    let f: SpillPoint = imp_for(&fwd, sel);
    let got = unsafe { f(this, sel, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, NSPoint::new(8.0, 9.0), 10.0) };
    assert_eq!(got, (1..=10).map(|n| (n * n) as f64).sum::<f64>());
    let sel = sel!(d:d:d:d:d:d:d:d:rect:);
    type SpillRect = unsafe extern "C-unwind" fn(Id, Sel, f64, f64, f64, f64, f64, f64, f64, f64, NSRect) -> NSRect;
    let f: SpillRect = imp_for(&fwd, sel);
    let rect = NSRect::new(NSPoint::new(0.5, 2.0), NSSize::new(3.0, 4.0));
    let got = unsafe { f(this, sel, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, rect) };
    assert_eq!(got, NSRect::new(NSPoint::new(36.5, 2.0), NSSize::new(3.0, 4.0)));
    let sel = sel!(wide:after:);
    let f: unsafe extern "C-unwind" fn(Id, Sel, Wide, f64) -> f64 = imp_for(&fwd, sel);
    assert_eq!(unsafe { f(this, sel, Wide { v: [1, 2, 3, 4, 5] }, 0.5) }, 15.5);

    let a = NSObject::new();
    let b = NSObject::new();
    let sel = sel!(first:second:);
    let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> Id = imp_for(&fwd, sel);
    let (pa, pb): (Id, Id) = (Retained::as_ptr(&a).cast(), Retained::as_ptr(&b).cast());
    assert_eq!(unsafe { f(this, sel, pa, pb) }, pb);
    let sel = sel!(echoSelector:);
    let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Sel = imp_for(&fwd, sel);
    assert_eq!(unsafe { f(this, sel, sel!(hash)) }, sel!(hash));
    let sel = sel!(store:into:);
    let f: unsafe extern "C-unwind" fn(Id, Sel, i64, *mut i64) = imp_for(&fwd, sel);
    let mut out = 0;
    unsafe { f(this, sel, 5, &mut out) };
    assert_eq!(out, 105);
}

#[test]
fn forwards_struct_arguments_and_returns() {
    let fwd = Forwarder::new(100);
    let this: Id = Retained::as_ptr(&fwd).cast();
    macro_rules! check {
        ($sel:expr, $ty:ty, $arg:expr, $want:expr) => {{
            let sel = $sel;
            let f: unsafe extern "C-unwind" fn(Id, Sel, $ty) -> $ty = imp_for(&fwd, sel);
            assert_eq!(unsafe { f(this, sel, $arg) }, $want, "{sel:?}");
        }};
    }
    check!(sel!(point:), NSPoint, NSPoint::new(1.5, 2.0), NSPoint::new(3.0, 102.0));
    let rect = NSRect::new(NSPoint::new(1.0, 2.0), NSSize::new(3.0, 4.0));
    check!(sel!(rect:), NSRect, rect, NSRect::new(NSPoint::new(2.0, 1.0), NSSize::new(6.0, 12.0)));
    check!(sel!(range:), NSRange, NSRange::new(5, 6), NSRange::new(6, 12));
    check!(sel!(mixed:), Mixed, Mixed { c: -1, i: 21 }, Mixed { c: -2, i: 42 });
    check!(sel!(float3:), Float3, Float3 { a: 1.0, b: 2.0, c: 3.0 }, Float3 { a: 3.0, b: 1.0, c: 2.5 });
    check!(sel!(doubleLong:), DoubleLong, DoubleLong { d: 1.25, q: 10 }, DoubleLong { d: 2.5, q: 9 });
    check!(sel!(floatInt:), FloatInt, FloatInt { f: 1.5, i: 4 }, FloatInt { f: 2.5, i: -4 });
    check!(sel!(int3:), Int3, Int3 { a: 1, b: 2, c: 3 }, Int3 { a: 3, b: 20, c: 1 });
    check!(sel!(bytes3:), Bytes3, Bytes3 { a: 1, b: 2, c: 255 }, Bytes3 { a: 255, b: 1, c: 3 });
    check!(sel!(wide:), Wide, Wide { v: [1, 2, 3, 4, 5] }, Wide { v: [105, 4, 3, 2, 1] });
    check!(
        sel!(doubles5:),
        Doubles5,
        Doubles5 { v: [1.0, 2.0, 3.0, 4.0, 5.0] },
        Doubles5 { v: [2.0, 4.0, 6.0, 8.0, 10.0] }
    );
}

#[test]
#[cfg(not(debug_assertions))]
fn msg_send_forwards_invocations() {
    let fwd = Forwarder::new(100);
    for _ in 0..3 {
        let got: NSPoint = unsafe { msg_send![&*fwd, point: NSPoint::new(1.0, 1.0)] };
        assert_eq!(got, NSPoint::new(2.0, 101.0));
        let got: Wide = unsafe { msg_send![&*fwd, wide: Wide { v: [0; 5] }] };
        assert_eq!(got.v[0], 100);
        let got: i64 = unsafe { msg_send![&*fwd, sidestepAnswer] };
        assert_eq!(got, 77);
    }
}

/// Without a signature, the message is unrecognized, reported through
/// the receiver's `-doesNotRecognizeSelector:`.
#[test]
fn no_signature_is_unrecognized() {
    let fwd = Forwarder::new(0);
    let f: unsafe extern "C-unwind" fn(Id, Sel) -> i64 = imp_for(&fwd, sel!(sidestepUnknownToEveryone));
    let reason = failure(|| {
        unsafe { f(Retained::as_ptr(&fwd).cast(), sel!(sidestepUnknownToEveryone)) };
    });
    assert!(reason.contains("-[SidestepInvForwarder sidestepUnknownToEveryone]: unrecognized selector"), "{reason}");
    assert_eq!(*fwd.ivars().log.borrow(), ["target", "signature"]);
}

struct RecorderIvars {
    recorded: RefCell<Vec<Retained<NSInvocation>>>,
    prototype: Retained<Callee>,
}

define_class!(
    /// Records the messages sent to it to replay later, as an undo
    /// manager's `prepareWithInvocationTarget:` proxy does.
    #[unsafe(super(NSObject))]
    #[name = "SidestepInvRecorder"]
    #[ivars = RecorderIvars]
    struct Recorder;

    impl Recorder {
        #[unsafe(method_id(methodSignatureForSelector:))]
        fn method_signature(&self, sel: Sel) -> Option<Retained<NSMethodSignature>> {
            unsafe { msg_send![&*self.ivars().prototype, methodSignatureForSelector: sel] }
        }

        #[unsafe(method(forwardInvocation:))]
        fn forward_invocation(&self, inv: &NSInvocation) {
            unsafe { inv.retainArguments() };
            self.ivars().recorded.borrow_mut().push(inv.retain());
        }
    }
);

#[test]
fn recorded_invocations_replay_later() {
    let this = Recorder::alloc().set_ivars(RecorderIvars { recorded: RefCell::default(), prototype: Callee::new(0) });
    let recorder: Retained<Recorder> = unsafe { msg_send![super(this), init] };
    static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
    autoreleasepool(|_| {
        let arg = tracked(&DEALLOCS);
        let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> Id = imp_for(&recorder, sel!(first:second:));
        let got = unsafe {
            f(Retained::as_ptr(&recorder).cast(), sel!(first:second:), Retained::as_ptr(&arg).cast(), std::ptr::null())
        };
        // Nothing ran: the return value is empty.
        assert!(got.is_null());
        let f: unsafe extern "C-unwind" fn(Id, Sel, NSRect) -> NSRect = imp_for(&recorder, sel!(rect:));
        let rect = NSRect::new(NSPoint::new(1.0, 2.0), NSSize::new(3.0, 4.0));
        assert_eq!(unsafe { f(Retained::as_ptr(&recorder).cast(), sel!(rect:), rect) }, NSRect::ZERO);
    });
    // The recorded invocation keeps its argument.
    assert_eq!(DEALLOCS.load(Ordering::SeqCst), 0);
    autoreleasepool(|_| {
        let target = Callee::new(0);
        let recorded = recorder.ivars().recorded.take();
        assert_eq!(recorded.len(), 2);
        unsafe { recorded[0].invokeWithTarget(&target) };
        let arg: Id = get(&recorded[0], 2);
        assert_eq!(returned::<Id>(&recorded[0]), arg);
        unsafe { recorded[1].invokeWithTarget(&target) };
        assert_eq!(returned::<NSRect>(&recorded[1]), NSRect::new(NSPoint::new(2.0, 1.0), NSSize::new(6.0, 12.0)));
    });
    assert_eq!(DEALLOCS.load(Ordering::SeqCst), 1);
}
