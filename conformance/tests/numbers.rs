//! `NSNumber` and `NSValue`, checked on macOS and on Linux alike.

use std::cmp::Ordering;

use std::ffi::{c_char, c_void};
use std::ptr::NonNull;

use objc2::encode::Encoding;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, define_class, msg_send};
use objc2_foundation::{
    NSComparisonResult, NSCopying, NSDictionary, NSEdgeInsets, NSNumber, NSPoint, NSRange, NSRect, NSSize, NSString,
    NSValue,
};

use sidestep as _;

fn description(obj: &AnyObject) -> String {
    let d: Retained<NSString> = unsafe { msg_send![obj, description] };
    d.to_string()
}

#[test]
fn numbers_report_foundations_c_types() {
    let cases: [(Retained<NSNumber>, Encoding); 16] = [
        (NSNumber::new_bool(true), Encoding::Char),
        (NSNumber::new_i8(-5), Encoding::Char),
        (NSNumber::new_u8(200), Encoding::Short),
        (NSNumber::new_i16(-300), Encoding::Short),
        (NSNumber::new_u16(60000), Encoding::Int),
        (NSNumber::new_i32(-70000), Encoding::Int),
        (NSNumber::new_u32(4_000_000_000), Encoding::LongLong),
        (NSNumber::new_i64(i64::MIN), Encoding::LongLong),
        (NSNumber::new_isize(-1), Encoding::LongLong),
        (NSNumber::new_u64(5), Encoding::LongLong),
        (NSNumber::new_u64(1 << 63), Encoding::ULongLong),
        (NSNumber::new_u64(u64::MAX), Encoding::ULongLong),
        (NSNumber::new_usize(7), Encoding::LongLong),
        (NSNumber::new_usize(usize::MAX), Encoding::ULongLong),
        (NSNumber::new_f32(1.5), Encoding::Float),
        (NSNumber::new_f64(1.5), Encoding::Double),
    ];
    for (i, (number, encoding)) in cases.iter().enumerate() {
        assert_eq!(number.encoding(), *encoding, "case {i}");
    }
}

#[test]
fn accessors_convert_like_c() {
    let n = NSNumber::new_u8(200);
    assert_eq!((n.as_u8(), n.as_i8(), n.as_i32(), n.as_f64()), (200, -56, 200, 200.0));
    let n = NSNumber::new_i32(256);
    assert_eq!((n.as_u8(), n.as_i8(), n.as_bool()), (0, 0, true));
    let n = NSNumber::new_i64(1 << 32);
    assert_eq!((n.as_i32(), n.as_bool()), (0, true));
    let n = NSNumber::new_i64(-1);
    assert_eq!((n.as_u64(), n.as_u32(), n.as_f64(), n.as_usize()), (u64::MAX, u32::MAX, -1.0, usize::MAX));
    let n = NSNumber::new_u64(u64::MAX);
    assert_eq!((n.as_i64(), n.as_u64(), n.as_bool()), (-1, u64::MAX, true));
    assert_eq!(n.as_f64(), u64::MAX as f64);
    let n = NSNumber::new_f64(3.99);
    assert_eq!((n.as_i32(), n.as_u32(), n.as_i8(), n.as_i64()), (3, 3, 3, 3));
    let n = NSNumber::new_f64(-3.99);
    assert_eq!((n.as_i32(), n.as_i64()), (-3, -3));
    assert_eq!(NSNumber::new_f64(1e20).as_i64(), i64::MAX);
    assert_eq!(NSNumber::new_f64(0.1).as_f32(), 0.1f32);
    assert_eq!(NSNumber::new_f32(0.1).as_f64(), 0.1f32 as f64);
    assert!(NSNumber::new_f64(0.4).as_bool());
    assert!(!NSNumber::new_f64(-0.0).as_bool());
    assert!(NSNumber::new_f64(f64::NAN).as_bool());
    assert!(!NSNumber::new_bool(false).as_bool());
    assert_eq!(NSNumber::new_bool(true).as_i64(), 1);
    assert_eq!(NSNumber::new_f64(12345.678).as_isize(), 12345);
}

#[test]
fn descriptions_are_c_formatted() {
    let cases: Vec<(Retained<NSNumber>, &str)> = vec![
        (NSNumber::new_i8(65), "65"),
        (NSNumber::new_i64(-9_223_372_036_854_775_808), "-9223372036854775808"),
        (NSNumber::new_u64(u64::MAX), "18446744073709551615"),
        (NSNumber::new_bool(true), "1"),
        (NSNumber::new_bool(false), "0"),
        (NSNumber::new_f64(1.0), "1"),
        (NSNumber::new_f64(0.1), "0.1"),
        (NSNumber::new_f64(-0.0), "-0"),
        (NSNumber::new_f64(0.1 + 0.2), "0.3"),
        (NSNumber::new_f64(1.0 / 3.0), "0.3333333333333333"),
        (NSNumber::new_f64(std::f64::consts::PI), "3.141592653589793"),
        (NSNumber::new_f64(12345.678), "12345.678"),
        (NSNumber::new_f64(1e15), "1000000000000000"),
        (NSNumber::new_f64(1e16), "1e+16"),
        (NSNumber::new_f64(123456789012345678.0), "1.234567890123457e+17"),
        (NSNumber::new_f64(1e-4), "0.0001"),
        (NSNumber::new_f64(1e-5), "1e-05"),
        (NSNumber::new_f64(0.0001234), "0.0001234"),
        (NSNumber::new_f64(f64::MAX), "1.797693134862316e+308"),
        (NSNumber::new_f64(5e-324), "4.940656458412465e-324"),
        (NSNumber::new_f64(f64::INFINITY), "inf"),
        (NSNumber::new_f64(f64::NEG_INFINITY), "-inf"),
        (NSNumber::new_f64(f64::NAN), "nan"),
        (NSNumber::new_f32(0.1), "0.1"),
        (NSNumber::new_f32(1.5), "1.5"),
        (NSNumber::new_f32(1e10), "1e+10"),
        (NSNumber::new_f32(16777217.0), "1.677722e+07"),
    ];
    for (number, text) in cases {
        assert_eq!(number.stringValue().to_string(), text);
        assert_eq!(description(&number), text);
        assert_eq!(number.to_string(), text);
    }
}

#[test]
fn equality_crosses_types() {
    let one = NSNumber::new_i64(1);
    for other in [NSNumber::new_f64(1.0), NSNumber::new_f32(1.0), NSNumber::new_bool(true), NSNumber::new_u8(1)] {
        assert!(one.isEqualToNumber(&other) && other.isEqualToNumber(&one));
        assert!(one.isEqual(Some(&other)));
        assert_eq!(one.hash(), other.hash(), "equal numbers hash alike");
        assert_eq!(one.compare(&other), NSComparisonResult::Same);
    }
    assert!(!NSNumber::new_f64(0.1).isEqualToNumber(&NSNumber::new_f32(0.1)));
    assert!(NSNumber::new_f32(0.1).isEqualToNumber(&NSNumber::new_f64(0.1f32 as f64)));
    assert!(!NSNumber::new_u64(u64::MAX).isEqualToNumber(&NSNumber::new_i64(-1)));
    assert_eq!(NSNumber::new_u64(u64::MAX).compare(&NSNumber::new_i64(-1)), NSComparisonResult::Descending);
    assert_eq!(NSNumber::new_i64(i64::MAX).compare(&NSNumber::new_u64(1 << 63)), NSComparisonResult::Ascending);
    assert!(NSNumber::new_i64(0).isEqualToNumber(&NSNumber::new_f64(-0.0)));
    assert_eq!(NSNumber::new_i64(0).hash(), NSNumber::new_f64(-0.0).hash());
    let nan = NSNumber::new_f64(f64::NAN);
    assert!(nan.isEqualToNumber(&NSNumber::new_f64(f64::NAN)));
    assert_eq!(nan.hash(), NSNumber::new_f32(f32::NAN).hash());
    assert!(!nan.isEqualToNumber(&NSNumber::new_i64(0)));
    assert_eq!(NSNumber::new_f64(2.5).compare(&NSNumber::new_i64(2)), NSComparisonResult::Descending);
    assert_eq!(NSNumber::new_i8(-5).cmp(&NSNumber::new_u8(251)), Ordering::Less);
    // Big integers compare as doubles against floating point.
    assert!(NSNumber::new_i64((1 << 53) + 1).isEqualToNumber(&NSNumber::new_f64((1u64 << 53) as f64)));
    assert!(!NSNumber::new_i64(1).isEqual(Some(&NSString::from_str("1"))));
    assert!(!NSNumber::new_i64(1).isEqual(None));
}

#[test]
fn numbers_are_dictionary_keys() {
    let keys = [NSNumber::new_i64(1), NSNumber::new_i64(2), NSNumber::new_f64(2.5)];
    let values = [NSString::from_str("one"), NSString::from_str("two"), NSString::from_str("two and a half")];
    let key_refs: Vec<&NSNumber> = keys.iter().map(|k| &**k).collect();
    let dict = NSDictionary::from_retained_objects(&key_refs, &values);
    assert_eq!(dict.objectForKey(&NSNumber::new_f64(1.0)).unwrap().to_string(), "one");
    assert_eq!(dict.objectForKey(&NSNumber::new_u8(2)).unwrap().to_string(), "two");
    assert_eq!(dict.objectForKey(&NSNumber::new_f32(2.5)).unwrap().to_string(), "two and a half");
    assert!(dict.objectForKey(&NSNumber::new_i64(3)).is_none());
}

#[test]
fn numbers_copy_as_themselves() {
    let n = NSNumber::new_i64(123_456_789_012);
    let copy = n.copy();
    assert!(copy.isEqualToNumber(&n));
}

#[test]
fn geometry_values_round_trip() {
    let point = NSPoint::new(1.5, -2.0);
    let v = unsafe { NSValue::valueWithPoint(point) };
    assert_eq!(unsafe { v.pointValue() }, point);
    assert!(v.contains_encoding::<NSPoint>());
    assert_eq!(v.get_point(), Some(point));
    assert_eq!(description(&v), "NSPoint: {1.5, -2}");

    let size = NSSize::new(3.0, 4.25);
    let v = unsafe { NSValue::valueWithSize(size) };
    assert_eq!(unsafe { v.sizeValue() }, size);
    assert_eq!(v.get_size(), Some(size));
    assert_eq!(description(&v), "NSSize: {3, 4.25}");

    let rect = NSRect::new(NSPoint::new(1.0, 2.0), NSSize::new(3.0, 4.0));
    let v = unsafe { NSValue::valueWithRect(rect) };
    assert_eq!(unsafe { v.rectValue() }, rect);
    assert_eq!(v.get_rect(), Some(rect));
    assert_eq!(description(&v), "NSRect: {{1, 2}, {3, 4}}");

    let range = NSRange::new(3, 4);
    let v = unsafe { NSValue::valueWithRange(range) };
    assert_eq!(unsafe { v.rangeValue() }, range);
    assert_eq!(v.get_range(), Some(range));
    assert_eq!(description(&v), "NSRange: {3, 4}");

    let insets = NSEdgeInsets { top: 1.0, left: 2.0, bottom: 3.0, right: 4.0 };
    let v = unsafe { NSValue::valueWithEdgeInsets(insets) };
    assert_eq!(unsafe { v.edgeInsetsValue() }, insets);
    assert_eq!(description(&v), "NSEdgeInsets: {1, 2, 3, 4}");

    let precise = unsafe { NSValue::valueWithPoint(NSPoint::new(0.1, 1.0 / 3.0)) };
    assert_eq!(description(&precise), "NSPoint: {0.10000000000000001, 0.33333333333333331}");
}

#[test]
fn values_compare_by_type_and_bytes() {
    let a = unsafe { NSValue::valueWithPoint(NSPoint::new(1.0, 2.0)) };
    let b = NSValue::new(NSPoint::new(1.0, 2.0));
    let size = unsafe { NSValue::valueWithSize(NSSize::new(1.0, 2.0)) };
    assert!(a.isEqualToValue(&b) && a.isEqual(Some(&b)));
    assert_eq!(a.hash(), b.hash());
    assert!(!a.isEqualToValue(&size), "same bytes, different type");
    assert_eq!(a, b);

    // A plain value equals a number with its type and bytes; a number only
    // equals numbers.
    let value = NSValue::new(1i64);
    let number = NSNumber::new_i64(1);
    assert!(value.isEqual(Some(&number)));
    assert!(!number.isEqual(Some(&value)));
    assert!(!value.isKindOfClass(NSNumber::class()));
}

#[test]
fn arbitrary_values() {
    let v = NSValue::new(42u32);
    assert_eq!(v.encoding(), Some("I"));
    assert_eq!(unsafe { v.get::<u32>() }, 42);
    assert_eq!(description(&v), "{length = 4, bytes = 0x2a000000}");
    let v = NSValue::new([1u8, 2, 3, 4]);
    assert_eq!(description(&v), "{length = 4, bytes = 0x01020304}");
    let v = NSValue::new(0x0102_0304_0506_0708u64);
    assert_eq!(unsafe { v.get::<u64>() }, 0x0102_0304_0506_0708);
    let bytes: [u8; 32] = std::array::from_fn(|i| i as u8);
    let v = NSValue::new(bytes);
    assert_eq!(unsafe { v.get::<[u8; 32]>() }, bytes);
    assert_eq!(description(&v), "{length = 32, bytes = 0x00010203 04050607 08090a0b 0c0d0e0f ... 18191a1b 1c1d1e1f }");
    let pointer = 0x1234usize as *const std::ffi::c_void;
    let v = unsafe { NSValue::valueWithPointer(pointer) };
    assert_eq!(unsafe { v.pointerValue() } as usize, 0x1234);
    assert_eq!(v.encoding(), Some("^v"));

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Mixed {
        c: u8,
        d: f64,
    }
    unsafe impl objc2::encode::Encode for Mixed {
        const ENCODING: Encoding = Encoding::Struct("Mixed", &[Encoding::UChar, Encoding::Double]);
    }
    let mixed = Mixed { c: 7, d: 2.5 };
    let v = NSValue::new(mixed);
    assert_eq!(unsafe { v.get::<Mixed>() }, mixed);

    // Equality compares every byte, padding included, on Apple's side as on
    // Sidestep's, so two copies of `Mixed` (whose padding holds whatever was
    // on the stack) may differ. Compare a struct without padding.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Pair {
        a: u32,
        b: f32,
    }
    unsafe impl objc2::encode::Encode for Pair {
        const ENCODING: Encoding = Encoding::Struct("Pair", &[Encoding::UInt, Encoding::Float]);
    }
    let pair = NSValue::new(Pair { a: 7, b: 2.5 });
    assert!(pair.isEqualToValue(&NSValue::new(Pair { a: 7, b: 2.5 })));
    assert!(!pair.isEqualToValue(&NSValue::new(Pair { a: 8, b: 2.5 })));
}

#[test]
fn values_need_contents() {
    let n: Option<Retained<NSNumber>> = unsafe { msg_send![NSNumber::alloc(), init] };
    assert!(n.is_none());
    let v: Option<Retained<NSValue>> = unsafe { msg_send![NSValue::alloc(), init] };
    assert!(v.is_none());
}

#[test]
fn values_from_bytes() {
    let value: i32 = 42;
    let pointer = std::ptr::NonNull::from(&value).cast();
    let type_i = std::ptr::NonNull::new(c"i".as_ptr().cast_mut()).unwrap();
    // Foundation makes a plain value here, Sidestep a number; both hold 42
    // as an `int`.
    let x = unsafe { NSNumber::initWithBytes_objCType(NSNumber::alloc(), pointer, type_i) };
    assert_eq!((**x).encoding(), Some("i"));
    assert_eq!(unsafe { x.get::<i32>() }, 42);
    let y = unsafe { NSValue::initWithBytes_objCType(NSValue::alloc(), pointer, type_i) };
    assert!(y.isEqualToValue(&x) && x.isEqualToValue(&y));
}

/// Every creation method and accessor through objc2's bindings, so that
/// debug builds check each one's type encoding against the binding's.
#[test]
fn api_surface() {
    let made = [
        NSNumber::numberWithChar(7),
        NSNumber::numberWithUnsignedChar(7),
        NSNumber::numberWithShort(7),
        NSNumber::numberWithUnsignedShort(7),
        NSNumber::numberWithInt(7),
        NSNumber::numberWithUnsignedInt(7),
        NSNumber::numberWithLong(7),
        NSNumber::numberWithUnsignedLong(7),
        NSNumber::numberWithLongLong(7),
        NSNumber::numberWithUnsignedLongLong(7),
        NSNumber::numberWithFloat(7.0),
        NSNumber::numberWithDouble(7.0),
        NSNumber::numberWithInteger(7),
        NSNumber::numberWithUnsignedInteger(7),
        NSNumber::initWithChar(NSNumber::alloc(), 7),
        NSNumber::initWithUnsignedChar(NSNumber::alloc(), 7),
        NSNumber::initWithShort(NSNumber::alloc(), 7),
        NSNumber::initWithUnsignedShort(NSNumber::alloc(), 7),
        NSNumber::initWithInt(NSNumber::alloc(), 7),
        NSNumber::initWithUnsignedInt(NSNumber::alloc(), 7),
        NSNumber::initWithLong(NSNumber::alloc(), 7),
        NSNumber::initWithUnsignedLong(NSNumber::alloc(), 7),
        NSNumber::initWithLongLong(NSNumber::alloc(), 7),
        NSNumber::initWithUnsignedLongLong(NSNumber::alloc(), 7),
        NSNumber::initWithFloat(NSNumber::alloc(), 7.0),
        NSNumber::initWithDouble(NSNumber::alloc(), 7.0),
        NSNumber::initWithInteger(NSNumber::alloc(), 7),
        NSNumber::initWithUnsignedInteger(NSNumber::alloc(), 7),
    ];
    for n in &made {
        assert_eq!(n.charValue(), 7);
        assert_eq!(n.unsignedCharValue(), 7);
        assert_eq!(n.shortValue(), 7);
        assert_eq!(n.unsignedShortValue(), 7);
        assert_eq!(n.intValue(), 7);
        assert_eq!(n.unsignedIntValue(), 7);
        assert_eq!(n.longValue(), 7);
        assert_eq!(n.unsignedLongValue(), 7);
        assert_eq!(n.longLongValue(), 7);
        assert_eq!(n.unsignedLongLongValue(), 7);
        assert_eq!(n.floatValue(), 7.0);
        assert_eq!(n.doubleValue(), 7.0);
        assert_eq!(n.integerValue(), 7);
        assert_eq!(n.unsignedIntegerValue(), 7);
        assert!(n.boolValue());
        assert_eq!(n.stringValue().to_string(), "7");
        let d: Retained<NSString> = unsafe { n.descriptionWithLocale(None) };
        assert_eq!(d.to_string(), "7");
        assert!(n.isEqualToNumber(&made[0]));
    }
    assert!(NSNumber::numberWithBool(true).boolValue());
    assert!(!NSNumber::initWithBool(NSNumber::alloc(), false).boolValue());

    let object = NSString::from_str("held, not retained");
    let v = unsafe { NSValue::valueWithNonretainedObject(Some(&object)) };
    let back = unsafe { v.nonretainedObjectValue() }.expect("object");
    assert!(std::ptr::eq(&*back as *const AnyObject, &*object as *const NSString as *const AnyObject));
    let mut out = 0u32;
    let v = NSValue::new(9u32);
    unsafe { v.getValue_size(std::ptr::NonNull::from(&mut out).cast(), 4) };
    assert_eq!(out, 9);
    let copy = v.copy();
    assert!(copy.isEqualToValue(&v));
    let value: u16 = 513;
    let bytes = NonNull::from(&value).cast::<c_void>();
    let type_s = NonNull::new(c"S".as_ptr().cast_mut()).unwrap();
    let made = unsafe { NSValue::valueWithBytes_objCType(bytes, type_s) };
    assert_eq!((made.encoding(), unsafe { made.get::<u16>() }), (Some("S"), 513));
    let made = unsafe { NSValue::value_withObjCType(bytes, type_s) };
    assert!(made.isEqualToValue(&NSValue::new(513u16)));
}

/// Out-of-range floating-point values through the unsigned 64-bit
/// accessors, which C leaves undefined: what Foundation gives on Apple's
/// hardware. Whole values wrap modulo 2^64; negative fractions go through
/// `value + 2^64` as a double.
#[test]
fn unsigned_accessors_of_out_of_range_doubles() {
    let cases: [(f64, u64, i64, u32); 16] = [
        (-0.5, u64::MAX, 0, 0),
        (-2.5, u64::MAX, -2, 4294967294),
        (-3.7, u64::MAX, -3, 4294967293),
        (-5000.5, 18446744073709547520, -5000, 4294962296),
        (1e20, 7766279631452241920, i64::MAX, u32::MAX),
        (-1e20, 10680464442257309696, i64::MIN, 0),
        (18446744073709551616.0, 0, i64::MAX, u32::MAX),
        (3e19, 11553255926290448384, i64::MAX, u32::MAX),
        (f64::INFINITY, u64::MAX, i64::MAX, u32::MAX),
        (f64::NEG_INFINITY, 0, i64::MIN, 0),
        (f64::NAN, 0, 0, 0),
        (-9.3e18, 9146744073709551616, i64::MIN, 0),
        (-3.0, 18446744073709551613, -3, 4294967293),
        (1.5e19, 15000000000000000000, i64::MAX, u32::MAX),
        (-1e30, 13369779918779449344, i64::MIN, 0),
        (1e30, 5076964154930102272, i64::MAX, u32::MAX),
    ];
    for (value, unsigned, signed, narrow) in cases {
        let number = NSNumber::new_f64(value);
        assert_eq!(number.as_u64(), unsigned, "{value:e} as u64");
        assert_eq!(number.as_usize() as u64, unsigned, "{value:e} as usize");
        assert_eq!(number.as_i64(), signed, "{value:e} as i64");
        assert_eq!(number.as_u32(), narrow, "{value:e} as u32");
    }
    for (value, unsigned) in [(-0.5f32, u64::MAX), (1e20, 7766281635539976192), (-5000.5, 18446744073709547520)] {
        assert_eq!(NSNumber::new_f32(value).as_u64(), unsigned, "{value:e}f32");
    }
}

#[test]
fn nonretained_objects_are_pointers() {
    let object = NSString::from_str("held, not retained");
    let held = unsafe { NSValue::valueWithNonretainedObject(Some(&object)) };
    let pointer = unsafe { NSValue::valueWithPointer(Retained::as_ptr(&object).cast()) };
    assert_eq!(held.encoding(), Some("^v"));
    assert!(held.isEqualToValue(&pointer) && pointer.isEqualToValue(&held));
    assert!(description(&held).starts_with("{length = 8, bytes = 0x"));
}

define_class!(
    /// A value defined outside the framework, through NSValue's two
    /// primitive methods: an `int` of 5.
    #[unsafe(super(NSValue))]
    #[name = "ConformanceFive"]
    struct Five;

    impl Five {
        #[unsafe(method(objCType))]
        fn objc_type(&self) -> NonNull<c_char> {
            NonNull::new(c"i".as_ptr().cast_mut()).unwrap()
        }

        #[unsafe(method(getValue:))]
        fn get_value(&self, value: NonNull<c_void>) {
            unsafe { value.cast::<i32>().write(5) };
        }
    }
);

#[test]
fn value_subclasses_compare_through_their_primitives() {
    let five: Retained<Five> = unsafe { msg_send![Five::alloc(), init] };
    let sub: &NSValue = &five;
    let plain = NSValue::new(5i32);
    assert!(plain.isEqualToValue(sub));
    assert!(plain.isEqual(Some(sub)));
    assert!(!NSValue::new(6i32).isEqualToValue(sub));
    assert!(!NSValue::new(5u32).isEqualToValue(sub), "the type counts");
}
