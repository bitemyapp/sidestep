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
//! How a double reads as a string was measured on macOS
//! (`conformance/tests/controls.rs`): up to 16 significant digits, with no
//! trailing zeros, and an exponent only for very large or small values.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, Message, msg_send};
use objc2_foundation::{NSAttributedString, NSCopying, NSNumber, NSString};

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
    /// keep their kind; everything else is kept as the object.
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
        Value::Object(object.retain())
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Value::Empty)
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

    /// `stringValue`.
    pub fn string(&self) -> Retained<NSString> {
        match self {
            Value::Empty => NSString::new(),
            Value::String(s) => s.clone(),
            Value::Attributed(a) => a.string(),
            Value::Int(i) => NSString::from_str(&i.to_string()),
            Value::Float(f) => NSString::from_str(&format_float(f64::from(*f), 7)),
            Value::Double(d) => NSString::from_str(&format_float(*d, 16)),
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

    /// `integerValue`; `intValue` is this, truncated.
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

    /// `intValue`.
    pub fn int(&self) -> i32 {
        match self {
            Value::String(s) => s.intValue(),
            Value::Attributed(a) => a.string().intValue(),
            Value::Double(d) => *d as i32,
            Value::Float(f) => *f as i32,
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

/// A floating-point value as AppKit's cells show one: `%.*G` with
/// `digits` significant digits, which drops trailing zeros and uses an
/// exponent only below 1e-4 or at `10^digits` and above.
pub(crate) fn format_float(value: f64, digits: usize) -> String {
    if value.is_nan() {
        return "NAN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "INF".into() } else { "-INF".into() };
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    // The exponent after rounding to `digits` significant digits.
    let sci = format!("{:.*e}", digits - 1, value);
    let (mantissa, exp) = sci.split_once('e').expect("an exponent");
    let exp: i32 = exp.parse().expect("an integer exponent");
    if exp < -4 || exp >= digits as i32 {
        let mantissa = trim_zeros(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}E{sign}{:02}", exp.unsigned_abs())
    } else {
        let decimals = (digits as i32 - 1 - exp).max(0) as usize;
        trim_zeros(&format!("{value:.decimals$}")).to_string()
    }
}

fn trim_zeros(s: &str) -> &str {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { s }
}

/// An empty attributed string, or one made of `text`, without attributes.
pub(crate) fn attributed(text: &NSString) -> Retained<NSAttributedString> {
    // SAFETY: initWithString: takes a string.
    unsafe { msg_send![NSAttributedString::alloc(), initWithString: text] }
}

#[cfg(test)]
mod tests {
    use super::format_float;

    #[test]
    fn doubles_read_as_macos_shows_them() {
        let cases = [
            (3.75, "3.75"),
            (0.1, "0.1"),
            (2.0, "2"),
            (1.0 / 3.0, "0.3333333333333333"),
            (-0.5, "-0.5"),
            (1e20, "1E+20"),
            (123456.0, "123456"),
            (1e-5, "1E-05"),
            (0.0001, "0.0001"),
        ];
        for (value, text) in cases {
            assert_eq!(format_float(value, 16), text, "{value}");
        }
        assert_eq!(format_float(f64::from(1.5f32), 7), "1.5");
        assert_eq!(format_float(f64::from(0.1f32), 7), "0.1");
    }
}
