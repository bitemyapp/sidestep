//! A cell's value, as `objectValue` and its typed siblings read and write
//! it.
//!
//! AppKit keeps one object value and converts on the way out: numbers set
//! with `setIntValue:` or `setDoubleValue:` come back as strings through
//! `stringValue`, strings come back as numbers through NSString's own
//! `intValue` and `doubleValue`, and an attributed string is the object
//! value itself. Numbers are kept unboxed here and become an `NSNumber`
//! only when a program asks for `objectValue`, so a slider or a stepper
//! changing value on every drag makes no object.
//!
//! A number reads as a string the way `NSNumber` describes itself in the
//! current locale (`descriptionWithLocale:`), which is what macOS shows
//! (`conformance/tests/controls.rs`, `cell_values`): "1,234.5" in English.
//! A float is widened to a double first, so 0.1 set as a float reads
//! "0.1000000014901161". The formatting is Foundation's; cells remember
//! the string they made until their value changes (see `cell`).
//!
//! `intValue` of a floating-point value goes through a 64-bit integer, as
//! C's conversions do on the hardware macOS runs on: 1e20 saturates to the
//! largest 64-bit integer, whose low 32 bits read -1.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, ClassType, Message, msg_send};
use objc2_foundation::{NSAttributedString, NSCopying, NSLocale, NSNumber, NSString};

/// What a cell holds.
#[derive(Clone, Debug, Default)]
pub(crate) enum Value {
    #[default]
    Empty,
    String(Retained<NSString>),
    Attributed(Retained<NSAttributedString>),
    Int(i64),
    Float(f32),
    Double(f64),
    /// Any other object: a number set with `setObjectValue:`, a date.
    Object(Retained<AnyObject>),
}

impl Value {
    /// The value from `setObjectValue:`. Strings and attributed strings
    /// keep their kind; everything else is kept as the object, copied as
    /// macOS copies it (`conformance/tests/controls.rs`,
    /// `values_that_change_their_cell`). An object that can't copy, which
    /// macOS refuses, is kept as it is.
    pub fn from_object(object: Option<&AnyObject>) -> Value {
        let Some(object) = object else { return Value::Empty };
        if let Some(s) = object.downcast_ref::<NSString>() {
            // Copied, as setters of string properties do: a mutable string
            // changed later doesn't change the cell.
            return Value::String(s.copy());
        }
        if let Some(a) = object.downcast_ref::<NSAttributedString>() {
            // SAFETY: copy returns an immutable attributed string.
            let copy: Retained<NSAttributedString> = unsafe { msg_send![a, copy] };
            return Value::Attributed(copy);
        }
        if responds(object, objc2::sel!(copyWithZone:)) {
            // SAFETY: copy returns a new or retained object.
            let copy: Retained<AnyObject> = unsafe { msg_send![object, copy] };
            return Value::Object(copy);
        }
        Value::Object(object.retain())
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Value::Empty)
    }

    /// Whether the value reads as an empty string, without making one for
    /// the values that can say so themselves.
    pub fn is_empty_text(&self) -> bool {
        match self {
            Value::Empty => true,
            Value::String(s) => s.length() == 0,
            Value::Attributed(a) => a.length() == 0,
            Value::Int(_) | Value::Float(_) | Value::Double(_) => false,
            Value::Object(o) => description(o).length() == 0,
        }
    }

    /// Whether storing `other` would change nothing a program can see: the
    /// same kind and an equal value (strings and attributed strings by
    /// their contents, other objects by identity or `isEqual:`).
    pub fn same(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Empty, Value::Empty) => true,
            (Value::String(a), Value::String(b)) => std::ptr::eq(&**a, &**b) || a.isEqualToString(b),
            (Value::Attributed(a), Value::Attributed(b)) => {
                // SAFETY: isEqualToAttributedString: takes an attributed
                // string and returns BOOL.
                std::ptr::eq(&**a, &**b) || unsafe { msg_send![&**a, isEqualToAttributedString: &**b] }
            }
            (Value::Int(a), Value::Int(b)) => a == b,
            // Bitwise, so that -0 differs from 0 and a NaN equals itself.
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Double(a), Value::Double(b)) => a.to_bits() == b.to_bits(),
            (Value::Object(a), Value::Object(b)) => {
                // SAFETY: isEqual: takes an object and returns BOOL.
                std::ptr::eq(&**a, &**b) || unsafe { msg_send![&**a, isEqual: &**b] }
            }
            _ => false,
        }
    }

    /// Whether the value is a number kept unboxed, whose string a cell may
    /// remember.
    pub fn is_number(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Float(_) | Value::Double(_))
    }

    /// `objectValue`: numbers become `NSNumber`s.
    pub fn object(&self) -> Option<Retained<AnyObject>> {
        match self {
            Value::Empty => None,
            Value::String(s) => Some(Retained::into_super(Retained::into_super(s.clone()))),
            Value::Attributed(a) => Some(Retained::into_super(Retained::into_super(a.clone()))),
            Value::Int(i) => Some(number(NSNumber::new_i64(*i))),
            Value::Float(f) => Some(number(NSNumber::new_f32(*f))),
            Value::Double(d) => Some(number(NSNumber::new_f64(*d))),
            Value::Object(o) => Some(o.clone()),
        }
    }

    /// `stringValue`. Numbers are formatted by Foundation (see the module
    /// documentation); cells call this once per value and keep the result.
    pub fn string(&self) -> Retained<NSString> {
        match self {
            Value::Empty => NSString::new(),
            Value::String(s) => s.clone(),
            Value::Attributed(a) => a.string(),
            Value::Int(i) => localized(&NSNumber::new_i64(*i)),
            Value::Float(f) => localized(&NSNumber::new_f64(f64::from(*f))),
            Value::Double(d) => localized(&NSNumber::new_f64(*d)),
            Value::Object(o) => description(o),
        }
    }

    /// `doubleValue`.
    pub fn double(&self) -> f64 {
        match self {
            Value::Empty => 0.0,
            Value::Int(i) => *i as f64,
            Value::Float(f) => f64::from(*f),
            Value::Double(d) => *d,
            Value::String(s) => s.doubleValue(),
            Value::Attributed(a) => a.string().doubleValue(),
            Value::Object(o) => send_double(o),
        }
    }

    /// `integerValue`.
    pub fn integer(&self) -> isize {
        match self {
            Value::Empty => 0,
            Value::Int(i) => *i as isize,
            // Truncated toward zero, saturating, as C's conversion is on the
            // hardware macOS runs on.
            Value::Float(f) => *f as isize,
            Value::Double(d) => *d as isize,
            Value::String(s) => s.integerValue(),
            Value::Attributed(a) => a.string().integerValue(),
            Value::Object(o) => send_integer(o),
        }
    }

    /// `intValue`: floating-point values through a 64-bit integer, then
    /// its low 32 bits (see the module documentation).
    pub fn int(&self) -> i32 {
        match self {
            Value::String(s) => s.intValue(),
            Value::Attributed(a) => a.string().intValue(),
            Value::Double(d) => (*d as i64) as i32,
            Value::Float(f) => (*f as i64) as i32,
            other => other.integer() as i32,
        }
    }

    /// `floatValue`.
    pub fn float(&self) -> f32 {
        match self {
            Value::Float(f) => *f,
            other => other.double() as f32,
        }
    }

    /// The value as an attributed string, if it is one.
    pub fn attributed(&self) -> Option<&NSAttributedString> {
        match self {
            Value::Attributed(a) => Some(a),
            _ => None,
        }
    }
}

fn number(n: Retained<NSNumber>) -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(Retained::into_super(n)))
}

/// A number as it describes itself in the current locale, which is asked
/// for once (a program's locale doesn't change under it).
fn localized(n: &NSNumber) -> Retained<NSString> {
    thread_local! {
        // SAFETY: +currentLocale takes nothing and returns a locale.
        static LOCALE: Retained<NSLocale> = unsafe { msg_send![NSLocale::class(), currentLocale] };
    }
    // SAFETY: descriptionWithLocale: takes a locale and returns a string.
    LOCALE.with(|locale| unsafe { msg_send![n, descriptionWithLocale: &**locale] })
}

/// An empty attributed string, or one made of `text`, without attributes.
pub(crate) fn attributed(text: &NSString) -> Retained<NSAttributedString> {
    // SAFETY: initWithString: takes a string.
    unsafe { msg_send![NSAttributedString::alloc(), initWithString: text] }
}

/// An object's `description`, the string any object reads as.
pub(crate) fn description(object: &AnyObject) -> Retained<NSString> {
    // SAFETY: description takes nothing and returns a string.
    unsafe { msg_send![object, description] }
}

fn responds(object: &AnyObject, sel: objc2::runtime::Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: sel] }
}

fn send_double(object: &AnyObject) -> f64 {
    if !responds(object, objc2::sel!(doubleValue)) {
        return 0.0;
    }
    // SAFETY: doubleValue takes nothing and returns a double.
    unsafe { msg_send![object, doubleValue] }
}

fn send_integer(object: &AnyObject) -> isize {
    if !responds(object, objc2::sel!(integerValue)) {
        return 0;
    }
    // SAFETY: integerValue takes nothing and returns an NSInteger.
    unsafe { msg_send![object, integerValue] }
}

#[cfg(test)]
mod tests {
    use super::Value;

    #[test]
    fn floating_point_values_read_as_ints_as_macos_converts_them() {
        let cases = [
            (3.75, 3, 3),
            (-3.75, -3, -3),
            // Past 32 bits: the low 32 bits of the 64-bit integer.
            (1e16, 1_874_919_424, 10_000_000_000_000_000),
            // Past 64 bits: saturated, then the low bits.
            (1e20, -1, isize::MAX),
            (f64::INFINITY, -1, isize::MAX),
            (f64::NEG_INFINITY, 0, isize::MIN),
            (f64::NAN, 0, 0),
        ];
        for (value, int, integer) in cases {
            let v = Value::Double(value);
            assert_eq!((v.int(), v.integer()), (int, integer), "{value}");
        }
        assert_eq!(Value::Float(1e20).int(), -1);
    }

    #[test]
    fn equal_values_are_the_same() {
        assert!(Value::Double(0.5).same(&Value::Double(0.5)));
        assert!(!Value::Double(0.0).same(&Value::Double(-0.0)));
        assert!(Value::Double(f64::NAN).same(&Value::Double(f64::NAN)));
        assert!(!Value::Int(1).same(&Value::Double(1.0)));
        assert!(Value::Empty.same(&Value::Empty));
    }
}
