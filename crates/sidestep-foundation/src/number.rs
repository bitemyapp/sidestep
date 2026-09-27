//! `NSNumber`: an immutable C number.
//!
//! A number keeps its value in the widest type of its kind (`i64`, `u64`,
//! `f32`, `f64`) and the C type it reports through `-objCType`, which
//! follows Foundation's: `char` and `BOOL` are `c`, `short` and `unsigned
//! char` are `s`, `int` and `unsigned short` are `i`, wider integers are `q`
//! unless they only fit in an `unsigned long long` (`Q`), and floating-point
//! numbers keep their own type.
//!
//! Comparison is by value across types, as in Foundation: if either side
//! is floating-point both compare as `double`, otherwise as exact integers.
//! So `1` equals `1.0`, and equal numbers hash alike because the hash is
//! taken from the value as a `double`. The accessors convert as C casts do.
//! Out-of-range conversions from floating point, which C leaves undefined,
//! follow what Foundation does on Apple's hardware, learned by running it:
//! the signed accessors saturate at the 64-bit limits and then truncate to
//! their width; the unsigned 64-bit ones take a whole value modulo 2^64 and
//! a negative fraction as `value + 2^64`, rounded as a `double` and then
//! saturated.

use std::cmp::Ordering;
use std::ffi::{CStr, c_char, c_long, c_ulong, c_void};
use std::fmt;
use std::ptr::{self, NonNull};
use std::sync::atomic::{self, AtomicPtr};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSComparisonResult, NSInteger, NSNumber, NSString, NSUInteger, NSValue, NSZone};

use sidestep_runtime::{ObjectRef, StaticObject};

use crate::util::is_exactly;

/// A number's value and C type.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Number {
    /// A signed integer, and the type it reports: `c`, `s`, `i` or `q`.
    Int(i64, u8),
    /// An unsigned integer above `i64::MAX` (`Q`).
    Big(u64),
    Float(f32),
    Double(f64),
}

impl Number {
    fn unsigned(value: u64) -> Number {
        match i64::try_from(value) {
            Ok(v) => Number::Int(v, b'q'),
            Err(_) => Number::Big(value),
        }
    }

    /// The `-objCType` string.
    fn objc_type(&self) -> &'static CStr {
        match self {
            Number::Int(_, b'c') => c"c",
            Number::Int(_, b's') => c"s",
            Number::Int(_, b'i') => c"i",
            Number::Int(..) => c"q",
            Number::Big(_) => c"Q",
            Number::Float(_) => c"f",
            Number::Double(_) => c"d",
        }
    }

    fn is_floating(&self) -> bool {
        matches!(self, Number::Float(_) | Number::Double(_))
    }

    pub(crate) fn as_i64(&self) -> i64 {
        match *self {
            Number::Int(v, _) => v,
            Number::Big(v) => v as i64,
            Number::Float(v) => v as i64,
            Number::Double(v) => v as i64,
        }
    }

    pub(crate) fn as_u64(&self) -> u64 {
        match *self {
            Number::Int(v, _) => v as u64,
            Number::Big(v) => v,
            Number::Float(v) => double_to_u64(v.into()),
            Number::Double(v) => double_to_u64(v),
        }
    }

    pub(crate) fn as_f64(&self) -> f64 {
        match *self {
            Number::Int(v, _) => v as f64,
            Number::Big(v) => v as f64,
            Number::Float(v) => v.into(),
            Number::Double(v) => v,
        }
    }

    fn as_f32(&self) -> f32 {
        match *self {
            Number::Int(v, _) => v as f32,
            Number::Big(v) => v as f32,
            Number::Float(v) => v,
            Number::Double(v) => v as f32,
        }
    }

    fn as_bool(&self) -> bool {
        match *self {
            Number::Int(v, _) => v != 0,
            Number::Big(_) => true,
            // NaN is true, as in C.
            Number::Float(v) => v != 0.0,
            Number::Double(v) => v != 0.0,
        }
    }

    /// `-compare:`.
    pub(crate) fn compare(&self, other: &Number) -> Ordering {
        if self.is_floating() || other.is_floating() {
            compare_doubles(self.as_f64(), other.as_f64())
        } else {
            self.as_i128().cmp(&other.as_i128())
        }
    }

    fn as_i128(&self) -> i128 {
        match *self {
            Number::Big(v) => v.into(),
            _ => self.as_i64().into(),
        }
    }

    pub(crate) fn equals(&self, other: &Number) -> bool {
        self.compare(other) == Ordering::Equal
    }

    /// Equal numbers are equal as doubles, so hashing the double keeps the
    /// hash consistent with `equals`. Whole values hash to themselves, so
    /// small integers spread well in tables.
    pub(crate) fn hash(&self) -> usize {
        let v = self.as_f64();
        if v.is_nan() {
            return 0;
        }
        let v = v.abs();
        if v < 18_446_744_073_709_551_616.0 && v.fract() == 0.0 { v as u64 as usize } else { v.to_bits() as usize }
    }

    /// The bytes of the value as its C type, for `-getValue:`.
    fn write_to(&self, out: *mut c_void) {
        // SAFETY: the caller of -getValue: passes room for the value's type.
        unsafe {
            match *self {
                Number::Int(v, b'c') => out.cast::<i8>().write_unaligned(v as i8),
                Number::Int(v, b's') => out.cast::<i16>().write_unaligned(v as i16),
                Number::Int(v, b'i') => out.cast::<i32>().write_unaligned(v as i32),
                Number::Int(v, _) => out.cast::<i64>().write_unaligned(v),
                Number::Big(v) => out.cast::<u64>().write_unaligned(v),
                Number::Float(v) => out.cast::<f32>().write_unaligned(v),
                Number::Double(v) => out.cast::<f64>().write_unaligned(v),
            }
        }
    }

    fn size(&self) -> usize {
        match self {
            Number::Int(_, b'c') => 1,
            Number::Int(_, b's') => 2,
            Number::Int(_, b'i') | Number::Float(_) => 4,
            _ => 8,
        }
    }

    /// The type and bytes of the value, as `-objCType` and `-getValue:`
    /// give them: the type and how many bytes of `bytes` hold the value.
    pub(crate) fn contents(&self, bytes: &mut [u8; 8]) -> (&'static CStr, usize) {
        self.write_to(bytes.as_mut_ptr().cast());
        (self.objc_type(), self.size())
    }
}

/// A `double` as `unsigned long long`, as Foundation converts one on
/// Apple's hardware (see the module notes).
fn double_to_u64(v: f64) -> u64 {
    const TWO_TO_64: f64 = 18_446_744_073_709_551_616.0;
    if v.is_nan() {
        0
    } else if v.is_infinite() || v.fract() != 0.0 {
        // Rust's float-to-int casts saturate, as the hardware's do.
        if v < 0.0 { (v + TWO_TO_64) as u64 } else { v as u64 }
    } else {
        // A whole number, taken modulo 2^64: its bits past the 64th fall
        // away.
        let bits = v.abs().to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as i32 - 1075;
        let mantissa = (bits & ((1 << 52) - 1)) | (1 << 52);
        let magnitude = match exponent {
            e if e <= -53 => 0,
            e if e < 0 => mantissa >> -e,
            e if e < 64 => mantissa.wrapping_shl(e as u32),
            _ => 0,
        };
        if v < 0.0 { magnitude.wrapping_neg() } else { magnitude }
    }
}

/// Doubles compare by value; NaN equals NaN and sorts between negative and
/// positive zero, as on Apple's Foundation.
fn compare_doubles(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
        (true, true) => Ordering::Equal,
        (true, false) => {
            if b.is_sign_negative() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (false, true) => compare_doubles(b, a).reverse(),
    }
}

/// `-description` and `-stringValue`: integers in decimal, `float` as
/// C's `%.7g` and `double` as `%.16g`.
impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Number::Int(v, _) => write!(f, "{v}"),
            Number::Big(v) => write!(f, "{v}"),
            Number::Float(v) => f.write_str(&format_g(v.into(), 7)),
            Number::Double(v) => f.write_str(&format_g(v, 16)),
        }
    }
}

/// C's `printf("%.{precision}g", value)`: the shorter of fixed and
/// scientific notation for `precision` significant digits, without
/// trailing zeros.
pub(crate) fn format_g(value: f64, precision: usize) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let precision = precision.max(1);
    // Rounded to `precision` digits first: the exponent C chooses by is the
    // one after rounding.
    let scientific = format!("{:.*e}", precision - 1, value);
    let (mantissa, exponent) = scientific.split_once('e').expect("exponent");
    let exponent: i32 = exponent.parse().expect("exponent");
    if exponent < -4 || exponent >= precision as i32 {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim_zeros(mantissa), exponent.unsigned_abs())
    } else {
        let decimals = (precision as i32 - 1 - exponent) as usize;
        trim_zeros(&format!("{value:.decimals$}")).to_owned()
    }
}

fn trim_zeros(s: &str) -> &str {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { s }
}

/// The value of one of Sidestep's own numbers, read without a message.
#[inline]
pub(crate) fn fast_value(obj: &AnyObject) -> Option<Number> {
    if is_exactly(obj, &crate::NSNUMBER) {
        // SAFETY: an instance of exactly NSNumberImpl.
        Some(*unsafe { &*(obj as *const AnyObject).cast::<NSNumberImpl>() }.ivars())
    } else {
        static_value(obj).copied()
    }
}

/// The value of any `NSNumber`, including subclasses defined elsewhere,
/// which are asked through their accessors; `None` for other objects.
pub(crate) fn value_of(obj: &AnyObject) -> Option<Number> {
    if let Some(n) = fast_value(obj) {
        return Some(n);
    }
    if !crate::util::is_kind(obj, NSNumber::class()) {
        return None;
    }
    // SAFETY: NSNumber's accessors take nothing and return what is named.
    unsafe {
        let objc_type: *const c_char = msg_send![obj, objCType];
        Some(match objc_type.cast::<u8>().as_ref() {
            Some(b'f') => Number::Float(msg_send![obj, floatValue]),
            Some(b'd') => Number::Double(msg_send![obj, doubleValue]),
            Some(b'Q') => Number::unsigned(msg_send![obj, unsignedLongLongValue]),
            Some(&t @ (b'c' | b's' | b'i')) => Number::Int(msg_send![obj, longLongValue], t),
            _ => Number::Int(msg_send![obj, longLongValue], b'q'),
        })
    }
}

/// Small integers are boxed far more than any other numbers (indices,
/// counts, enumeration values, booleans), so `+numberWith...:` keeps one
/// object for each of these per C type, made on first use and kept for
/// the life of the process. Creating one of them is then a retain. (Apple's
/// Foundation stores such numbers in tagged pointers, which cost nothing.)
const CACHED: std::ops::RangeInclusive<i64> = -16..=1023;
const CACHED_LEN: usize = (*CACHED.end() - *CACHED.start() + 1) as usize;
/// By C type: `c`, `s`, `i` and `q`.
static CACHE: [[AtomicPtr<NSNumberImpl>; CACHED_LEN]; 4] =
    [const { [const { AtomicPtr::new(ptr::null_mut()) }; CACHED_LEN] }; 4];

/// A number object with `value`, as `+numberWith...:` returns it: not
/// retained for the caller. A shared small integer needs nothing more, the
/// cache keeps it alive; a new number is autoreleased.
fn returned(value: Number) -> *mut NSNumberImpl {
    match shared(value) {
        Some(shared) => shared,
        None => Retained::autorelease_return(make_new(value)),
    }
}

/// The shared object for a small integer, unretained, made on first use.
fn shared(value: Number) -> Option<*mut NSNumberImpl> {
    let Number::Int(v, objc_type) = value else { return None };
    if !CACHED.contains(&v) {
        return None;
    }
    let row = match objc_type {
        b'c' => 0,
        b's' => 1,
        b'i' => 2,
        _ => 3,
    };
    let slot = &CACHE[row][(v - CACHED.start()) as usize];
    let shared = slot.load(atomic::Ordering::Acquire);
    if !shared.is_null() {
        return Some(shared);
    }
    // The cache keeps this reference forever. Numbers are immutable, so
    // sharing them across threads is sound.
    let fresh = Retained::into_raw(make_new(value));
    match slot.compare_exchange(ptr::null_mut(), fresh, atomic::Ordering::AcqRel, atomic::Ordering::Acquire) {
        Ok(_) => Some(fresh),
        Err(first) => {
            // Another thread made it first; ours goes.
            // SAFETY: `fresh` came from `into_raw` just above.
            drop(unsafe { Retained::from_raw(fresh) });
            Some(first)
        }
    }
}

/// A new number object.
fn make_new(value: Number) -> Retained<NSNumberImpl> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSNumber::alloc();
    // SAFETY: NSNumber's class is NSNumberImpl, and an `Allocated` is a
    // pointer to its object whatever its type parameter.
    let this = unsafe { std::mem::transmute::<Allocated<NSNumber>, Allocated<NSNumberImpl>>(this) };
    init(this, value)
}

fn init(this: Allocated<NSNumberImpl>, value: Number) -> Retained<NSNumberImpl> {
    let this = this.set_ivars(value);
    // SAFETY: NSValue's initializer for subclasses, which leaves its own
    // storage empty.
    unsafe { msg_send![super(this), init] }
}

/// The C number of type `objc_type` at `value`.
///
/// # Safety
/// `value` must point to a value of that type.
// `long` is 32 bits on some targets, where the conversions aren't useless.
#[allow(clippy::useless_conversion)]
unsafe fn read_c_number(value: *const c_void, objc_type: &CStr) -> Number {
    // SAFETY: guaranteed by the caller; reads are unaligned-tolerant.
    unsafe {
        match objc_type.to_bytes() {
            b"c" => Number::Int(value.cast::<i8>().read_unaligned().into(), b'c'),
            b"B" => Number::Int(value.cast::<u8>().read_unaligned().into(), b'c'),
            b"C" => Number::Int(value.cast::<u8>().read_unaligned().into(), b's'),
            b"s" => Number::Int(value.cast::<i16>().read_unaligned().into(), b's'),
            b"S" => Number::Int(value.cast::<u16>().read_unaligned().into(), b'i'),
            b"i" => Number::Int(value.cast::<i32>().read_unaligned().into(), b'i'),
            b"I" => Number::Int(value.cast::<u32>().read_unaligned().into(), b'q'),
            b"l" => Number::Int(i64::from(value.cast::<c_long>().read_unaligned()), b'q'),
            b"L" => Number::unsigned(u64::from(value.cast::<c_ulong>().read_unaligned())),
            b"q" => Number::Int(value.cast::<i64>().read_unaligned(), b'q'),
            b"Q" => Number::unsigned(value.cast::<u64>().read_unaligned()),
            b"f" => Number::Float(value.cast::<f32>().read_unaligned()),
            b"d" => Number::Double(value.cast::<f64>().read_unaligned()),
            other => panic!(
                "*** -[NSNumber initWithBytes:objCType:]: {} is not a C number type",
                String::from_utf8_lossy(other)
            ),
        }
    }
}

fn comparison(order: Ordering) -> NSComparisonResult {
    order.into()
}

fn other_value(other: &NSNumber) -> Number {
    value_of(other).unwrap_or(Number::Int(0, b'q'))
}

// The booleans: `kCFBooleanTrue` and `kCFBooleanFalse`, which
// `+numberWithBool:` returns, as Foundation's does. They are numbers of C
// type `c` whose class is a private subclass of NSNumber (so `[@YES class]`
// tells a boolean from a `char`, as on macOS), living in static memory:
// data symbols must point at objects before any code runs. Being static,
// they have no storage for NSNumber's instance variables; NSNumber's
// methods read their value through `number()`, which knows them.

sidestep_runtime::static_class!(pub(crate) BOOLEAN_CLASS, BOOLEAN_META = "_SidestepBoolean", || {
    let _ = BooleanImpl::class();
});

define_class!(
    /// The class of the two boolean constants. NSNumber's methods serve it.
    #[unsafe(super(NSNumber, NSValue, objc2::runtime::NSObject))]
    #[name = "_SidestepBoolean"]
    struct BooleanImpl;
);

static TRUE: StaticObject<()> = StaticObject::new(&BOOLEAN_CLASS, ());
static FALSE: StaticObject<()> = StaticObject::new(&BOOLEAN_CLASS, ());
static TRUE_VALUE: Number = Number::Int(1, b'c');
static FALSE_VALUE: Number = Number::Int(0, b'c');

#[unsafe(no_mangle)]
pub static kCFBooleanTrue: ObjectRef = TRUE.object_ref();
#[unsafe(no_mangle)]
pub static kCFBooleanFalse: ObjectRef = FALSE.object_ref();

// CoreFoundation's other number constants, `kCFNumberNaN` and the
// infinities: doubles, static the same way, of another private subclass.

sidestep_runtime::static_class!(pub(crate) NUMBER_CONSTANT_CLASS, NUMBER_CONSTANT_META = "_SidestepNumberConstant", || {
    let _ = NumberConstantImpl::class();
});

define_class!(
    /// The class of CoreFoundation's NaN and infinity constants.
    #[unsafe(super(NSNumber, NSValue, objc2::runtime::NSObject))]
    #[name = "_SidestepNumberConstant"]
    struct NumberConstantImpl;
);

static NAN: StaticObject<()> = StaticObject::new(&NUMBER_CONSTANT_CLASS, ());
static POSITIVE_INFINITY: StaticObject<()> = StaticObject::new(&NUMBER_CONSTANT_CLASS, ());
static NEGATIVE_INFINITY: StaticObject<()> = StaticObject::new(&NUMBER_CONSTANT_CLASS, ());
static NAN_VALUE: Number = Number::Double(f64::NAN);
static POSITIVE_INFINITY_VALUE: Number = Number::Double(f64::INFINITY);
static NEGATIVE_INFINITY_VALUE: Number = Number::Double(f64::NEG_INFINITY);

#[unsafe(no_mangle)]
pub static kCFNumberNaN: ObjectRef = NAN.object_ref();
#[unsafe(no_mangle)]
pub static kCFNumberPositiveInfinity: ObjectRef = POSITIVE_INFINITY.object_ref();
#[unsafe(no_mangle)]
pub static kCFNumberNegativeInfinity: ObjectRef = NEGATIVE_INFINITY.object_ref();

fn boolean(value: bool) -> &'static StaticObject<()> {
    if value { &TRUE } else { &FALSE }
}

/// Whether `obj` is one of the boolean constants.
pub(crate) fn is_boolean(obj: &AnyObject) -> bool {
    let obj = (obj as *const AnyObject).cast_mut().cast();
    obj == TRUE.as_object() || obj == FALSE.as_object()
}

/// The value of one of the static numbers (the booleans and the
/// NaN and infinity constants); `None` for other objects.
#[inline]
fn static_value(obj: &AnyObject) -> Option<&'static Number> {
    let obj = (obj as *const AnyObject).cast_mut().cast();
    if obj == TRUE.as_object() {
        Some(&TRUE_VALUE)
    } else if obj == FALSE.as_object() {
        Some(&FALSE_VALUE)
    } else if obj == NAN.as_object() {
        Some(&NAN_VALUE)
    } else if obj == POSITIVE_INFINITY.as_object() {
        Some(&POSITIVE_INFINITY_VALUE)
    } else if obj == NEGATIVE_INFINITY.as_object() {
        Some(&NEGATIVE_INFINITY_VALUE)
    } else {
        None
    }
}

impl NSNumberImpl {
    /// The number's value: its ivars, or a static number's.
    #[inline]
    fn number(&self) -> &Number {
        match static_value(self) {
            Some(value) => value,
            None => self.ivars(),
        }
    }
}

define_class!(
    #[unsafe(super(NSValue, objc2::runtime::NSObject))]
    #[name = "NSNumber"]
    #[ivars = Number]
    pub(crate) struct NSNumberImpl;

    impl NSNumberImpl {
        #[unsafe(method(numberWithChar:))]
        fn with_char(value: c_char) -> *mut Self {
            returned(Number::Int(value as i8 as i64, b'c'))
        }

        #[unsafe(method(numberWithUnsignedChar:))]
        fn with_unsigned_char(value: u8) -> *mut Self {
            returned(Number::Int(value.into(), b's'))
        }

        #[unsafe(method(numberWithShort:))]
        fn with_short(value: i16) -> *mut Self {
            returned(Number::Int(value.into(), b's'))
        }

        #[unsafe(method(numberWithUnsignedShort:))]
        fn with_unsigned_short(value: u16) -> *mut Self {
            returned(Number::Int(value.into(), b'i'))
        }

        #[unsafe(method(numberWithInt:))]
        fn with_int(value: i32) -> *mut Self {
            returned(Number::Int(value.into(), b'i'))
        }

        #[unsafe(method(numberWithUnsignedInt:))]
        fn with_unsigned_int(value: u32) -> *mut Self {
            returned(Number::Int(value.into(), b'q'))
        }

        #[unsafe(method(numberWithLong:))]
        fn with_long(value: c_long) -> *mut Self {
            returned(Number::Int(value as i64, b'q'))
        }

        #[unsafe(method(numberWithUnsignedLong:))]
        fn with_unsigned_long(value: c_ulong) -> *mut Self {
            returned(Number::unsigned(value as u64))
        }

        #[unsafe(method(numberWithLongLong:))]
        fn with_long_long(value: i64) -> *mut Self {
            returned(Number::Int(value, b'q'))
        }

        #[unsafe(method(numberWithUnsignedLongLong:))]
        fn with_unsigned_long_long(value: u64) -> *mut Self {
            returned(Number::unsigned(value))
        }

        #[unsafe(method(numberWithFloat:))]
        fn with_float(value: f32) -> *mut Self {
            returned(Number::Float(value))
        }

        #[unsafe(method(numberWithDouble:))]
        fn with_double(value: f64) -> *mut Self {
            returned(Number::Double(value))
        }

        /// One of the two boolean constants, as in Foundation.
        #[unsafe(method(numberWithBool:))]
        fn with_bool(value: bool) -> *mut Self {
            boolean(value).as_object().cast()
        }

        #[unsafe(method(numberWithInteger:))]
        fn with_integer(value: NSInteger) -> *mut Self {
            returned(Number::Int(value as i64, b'q'))
        }

        #[unsafe(method(numberWithUnsignedInteger:))]
        fn with_unsigned_integer(value: NSUInteger) -> *mut Self {
            returned(Number::unsigned(value as u64))
        }

        #[unsafe(method_id(initWithChar:))]
        fn init_with_char(this: Allocated<Self>, value: c_char) -> Retained<Self> {
            init(this, Number::Int(value as i8 as i64, b'c'))
        }

        #[unsafe(method_id(initWithUnsignedChar:))]
        fn init_with_unsigned_char(this: Allocated<Self>, value: u8) -> Retained<Self> {
            init(this, Number::Int(value.into(), b's'))
        }

        #[unsafe(method_id(initWithShort:))]
        fn init_with_short(this: Allocated<Self>, value: i16) -> Retained<Self> {
            init(this, Number::Int(value.into(), b's'))
        }

        #[unsafe(method_id(initWithUnsignedShort:))]
        fn init_with_unsigned_short(this: Allocated<Self>, value: u16) -> Retained<Self> {
            init(this, Number::Int(value.into(), b'i'))
        }

        #[unsafe(method_id(initWithInt:))]
        fn init_with_int(this: Allocated<Self>, value: i32) -> Retained<Self> {
            init(this, Number::Int(value.into(), b'i'))
        }

        #[unsafe(method_id(initWithUnsignedInt:))]
        fn init_with_unsigned_int(this: Allocated<Self>, value: u32) -> Retained<Self> {
            init(this, Number::Int(value.into(), b'q'))
        }

        #[unsafe(method_id(initWithLong:))]
        fn init_with_long(this: Allocated<Self>, value: c_long) -> Retained<Self> {
            init(this, Number::Int(value as i64, b'q'))
        }

        #[unsafe(method_id(initWithUnsignedLong:))]
        fn init_with_unsigned_long(this: Allocated<Self>, value: c_ulong) -> Retained<Self> {
            init(this, Number::unsigned(value as u64))
        }

        #[unsafe(method_id(initWithLongLong:))]
        fn init_with_long_long(this: Allocated<Self>, value: i64) -> Retained<Self> {
            init(this, Number::Int(value, b'q'))
        }

        #[unsafe(method_id(initWithUnsignedLongLong:))]
        fn init_with_unsigned_long_long(this: Allocated<Self>, value: u64) -> Retained<Self> {
            init(this, Number::unsigned(value))
        }

        #[unsafe(method_id(initWithFloat:))]
        fn init_with_float(this: Allocated<Self>, value: f32) -> Retained<Self> {
            init(this, Number::Float(value))
        }

        #[unsafe(method_id(initWithDouble:))]
        fn init_with_double(this: Allocated<Self>, value: f64) -> Retained<Self> {
            init(this, Number::Double(value))
        }

        /// One of the two boolean constants, as `+numberWithBool:`.
        #[unsafe(method_id(initWithBool:))]
        fn init_with_bool(this: Allocated<Self>, value: bool) -> Retained<Self> {
            drop(this);
            // SAFETY: the constants are immortal numbers; retaining one is
            // a no-op.
            unsafe { Retained::retain(boolean(value).as_object().cast()) }.expect("static object")
        }

        #[unsafe(method_id(initWithInteger:))]
        fn init_with_integer(this: Allocated<Self>, value: NSInteger) -> Retained<Self> {
            init(this, Number::Int(value as i64, b'q'))
        }

        #[unsafe(method_id(initWithUnsignedInteger:))]
        fn init_with_unsigned_integer(this: Allocated<Self>, value: NSUInteger) -> Retained<Self> {
            init(this, Number::unsigned(value as u64))
        }

        #[unsafe(method(charValue))]
        fn char_value(&self) -> c_char {
            self.number().as_i64() as c_char
        }

        #[unsafe(method(unsignedCharValue))]
        fn unsigned_char_value(&self) -> u8 {
            self.number().as_i64() as u8
        }

        #[unsafe(method(shortValue))]
        fn short_value(&self) -> i16 {
            self.number().as_i64() as i16
        }

        #[unsafe(method(unsignedShortValue))]
        fn unsigned_short_value(&self) -> u16 {
            self.number().as_i64() as u16
        }

        #[unsafe(method(intValue))]
        fn int_value(&self) -> i32 {
            self.number().as_i64() as i32
        }

        #[unsafe(method(unsignedIntValue))]
        fn unsigned_int_value(&self) -> u32 {
            self.number().as_i64() as u32
        }

        #[unsafe(method(longValue))]
        fn long_value(&self) -> c_long {
            self.number().as_i64() as c_long
        }

        #[unsafe(method(unsignedLongValue))]
        fn unsigned_long_value(&self) -> c_ulong {
            self.number().as_u64() as c_ulong
        }

        #[unsafe(method(longLongValue))]
        fn long_long_value(&self) -> i64 {
            self.number().as_i64()
        }

        #[unsafe(method(unsignedLongLongValue))]
        fn unsigned_long_long_value(&self) -> u64 {
            self.number().as_u64()
        }

        #[unsafe(method(floatValue))]
        fn float_value(&self) -> f32 {
            self.number().as_f32()
        }

        #[unsafe(method(doubleValue))]
        fn double_value(&self) -> f64 {
            self.number().as_f64()
        }

        #[unsafe(method(boolValue))]
        fn bool_value(&self) -> bool {
            self.number().as_bool()
        }

        #[unsafe(method(integerValue))]
        fn integer_value(&self) -> NSInteger {
            self.number().as_i64() as NSInteger
        }

        #[unsafe(method(unsignedIntegerValue))]
        fn unsigned_integer_value(&self) -> NSUInteger {
            self.number().as_u64() as NSUInteger
        }

        #[unsafe(method_id(stringValue))]
        fn string_value(&self) -> Retained<NSString> {
            NSString::from_str(&self.number().to_string())
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&self.number().to_string())
        }

        /// Sidestep has no locales yet: numbers print as in the C locale.
        #[unsafe(method_id(descriptionWithLocale:))]
        fn description_with_locale(&self, _locale: Option<&AnyObject>) -> Retained<NSString> {
            NSString::from_str(&self.number().to_string())
        }

        #[unsafe(method(compare:))]
        fn compare(&self, other: &NSNumber) -> NSComparisonResult {
            comparison(self.number().compare(&other_value(other)))
        }

        #[unsafe(method(isEqualToNumber:))]
        fn is_equal_to_number(&self, other: &NSNumber) -> bool {
            self.number().equals(&other_value(other))
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(value_of).is_some_and(|other| self.number().equals(&other))
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            self.number().hash()
        }

        #[unsafe(method(objCType))]
        fn objc_type(&self) -> NonNull<c_char> {
            NonNull::new(self.number().objc_type().as_ptr().cast_mut()).expect("static string")
        }

        #[unsafe(method(getValue:))]
        fn get_value(&self, value: NonNull<c_void>) {
            self.number().write_to(value.as_ptr());
        }

        #[unsafe(method(getValue:size:))]
        fn get_value_size(&self, value: NonNull<c_void>, size: NSUInteger) {
            let number = self.number();
            if size != number.size() {
                panic!(
                    "Cannot get value with size {size}. The type encoded as {} is expected to be {} bytes",
                    number.objc_type().to_string_lossy(),
                    number.size()
                );
            }
            number.write_to(value.as_ptr());
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            // Immutable: a copy is the same object.
            self.retain()
        }

        /// A number needs a value: plain `-init` gives nil, as in
        /// Foundation.
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Option<Retained<Self>> {
            drop(this);
            None
        }

        /// A number from a C number of the given type. (Foundation makes a
        /// plain `NSValue` here, which an initializer defined with objc2
        /// can't return; the number holds the same value.)
        #[unsafe(method_id(initWithBytes:objCType:))]
        fn init_with_bytes(this: Allocated<Self>, value: NonNull<c_void>, objc_type: NonNull<c_char>) -> Retained<Self> {
            // SAFETY: the caller passes a C string and a value of that type.
            let number = unsafe { read_c_number(value.as_ptr(), CStr::from_ptr(objc_type.as_ptr())) };
            init(this, number)
        }

        /// Another number by value; any other value by type and bytes, as
        /// values compare.
        #[unsafe(method(isEqualToValue:))]
        fn is_equal_to_value(&self, other: &NSValue) -> bool {
            let number = self.number();
            match value_of(other) {
                Some(other) => number.equals(&other),
                None => {
                    let mut bytes = [0u8; 8];
                    let (objc_type, size) = number.contents(&mut bytes);
                    crate::value::with_contents(other, |theirs| theirs == Some((objc_type, &bytes[..size])))
                }
            }
        }
    }

    unsafe impl NSObjectProtocol for NSNumberImpl {}
);
