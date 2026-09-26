//! `doubleValue`, `intValue` and the other number accessors.
//!
//! Each skips leading spaces and tabs (not newlines), then reads as much of
//! a number as it can: decimal digits of any script, with a sign, and for
//! the floating-point forms a fraction and an exponent. Nothing else is a
//! number: no hexadecimal, `inf` or `nan`. Integers saturate at the limits
//! of their type. As macOS does, the integer forms also skip spaces between
//! the sign and the digits.

use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class};
use objc2_foundation::NSInteger;

use icu_properties::props::GeneralCategory;

use super::fold::general_category;
use super::view::view;
use super::wtf8;

/// The value of a decimal digit of any script. Unicode lays every script's
/// digits out as runs of ten starting at zero.
pub(crate) fn digit(c: u32) -> Option<u32> {
    if (0x30..=0x39).contains(&c) {
        return Some(c - 0x30);
    }
    if c < 0x80 || general_category(c) != GeneralCategory::DecimalNumber {
        return None;
    }
    let mut start = c;
    while start > 0 && general_category(start - 1) == GeneralCategory::DecimalNumber {
        start -= 1;
    }
    Some((c - start) % 10)
}

fn is_blank(c: u32) -> bool {
    c == 0x09 || (c != 0x0A && general_category(c) == GeneralCategory::SpaceSeparator)
}

/// A string's code points.
fn points(obj: &AnyObject) -> Vec<u32> {
    let v = view(obj);
    wtf8::code_points(v.text().bytes).map(|(_, c)| c).collect()
}

fn skip_blanks(p: &[u32]) -> &[u32] {
    let n = p.iter().take_while(|&&c| is_blank(c)).count();
    &p[n..]
}

/// The leading number as a double.
fn double(obj: &AnyObject) -> f64 {
    let p = points(obj);
    let p = skip_blanks(&p);
    // Rewrite the number's characters as ASCII and let std parse it.
    let mut s = String::new();
    let mut i = 0;
    if let Some(&c) = p.first()
        && (c == u32::from('+') || c == u32::from('-'))
    {
        s.push(c as u8 as char);
        i = 1;
    }
    let mut digits = 0;
    while let Some(d) = p.get(i).and_then(|&c| digit(c)) {
        s.push(char::from_digit(d, 10).expect("a digit"));
        digits += 1;
        i += 1;
    }
    if p.get(i) == Some(&u32::from('.')) {
        s.push('.');
        i += 1;
        while let Some(d) = p.get(i).and_then(|&c| digit(c)) {
            s.push(char::from_digit(d, 10).expect("a digit"));
            digits += 1;
            i += 1;
        }
    }
    if digits == 0 {
        return 0.0;
    }
    if let Some(&e) = p.get(i)
        && (e == u32::from('e') || e == u32::from('E'))
    {
        let mut exp = String::from("e");
        let mut j = i + 1;
        if let Some(&c) = p.get(j)
            && (c == u32::from('+') || c == u32::from('-'))
        {
            exp.push(c as u8 as char);
            j += 1;
        }
        let start = exp.len();
        while let Some(d) = p.get(j).and_then(|&c| digit(c)) {
            exp.push(char::from_digit(d, 10).expect("a digit"));
            j += 1;
        }
        if exp.len() > start {
            s.push_str(&exp);
        }
    }
    s.parse().unwrap_or(0.0)
}

/// The leading integer, saturated to `min..=max`.
fn integer(obj: &AnyObject, min: i128, max: i128) -> i128 {
    let p = points(obj);
    let mut p = skip_blanks(&p);
    let mut negative = false;
    if let Some(&c) = p.first()
        && (c == u32::from('+') || c == u32::from('-'))
    {
        negative = c == u32::from('-');
        p = skip_blanks(&p[1..]);
    }
    let mut value: i128 = 0;
    for &c in p {
        let Some(d) = digit(c) else { break };
        value = (value * 10 + i128::from(d)).min(i128::from(u64::MAX) * 2);
    }
    let value = if negative { -value } else { value };
    value.clamp(min, max)
}

fn this(obj: &Helper) -> &AnyObject {
    obj
}

define_class!(
    // NSString's number accessors, copied onto NSString when it loads.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepStringNumbers"]
    pub(crate) struct Helper;

    impl Helper {
        #[unsafe(method(doubleValue))]
        fn double_value(&self) -> f64 {
            double(this(self))
        }

        #[unsafe(method(floatValue))]
        fn float_value(&self) -> f32 {
            double(this(self)) as f32
        }

        #[unsafe(method(intValue))]
        fn int_value(&self) -> i32 {
            integer(this(self), i32::MIN.into(), i32::MAX.into()) as i32
        }

        #[unsafe(method(integerValue))]
        fn integer_value(&self) -> NSInteger {
            integer(this(self), NSInteger::MIN as i128, NSInteger::MAX as i128) as NSInteger
        }

        #[unsafe(method(longLongValue))]
        fn long_long_value(&self) -> i64 {
            integer(this(self), i64::MIN.into(), i64::MAX.into()) as i64
        }

        #[unsafe(method(boolValue))]
        fn bool_value(&self) -> bool {
            let p = points(this(self));
            let p = skip_blanks(&p);
            match p.first().map(|&c| char::from_u32(c).unwrap_or('\0')) {
                Some('Y' | 'y' | 'T' | 't') => true,
                _ => {
                    let p = match p.first() {
                        Some(&c) if c == u32::from('+') || c == u32::from('-') => &p[1..],
                        _ => p,
                    };
                    let p = &p[p.iter().take_while(|&&c| c == u32::from('0')).count()..];
                    p.first().is_some_and(|&c| (0x31..=0x39).contains(&c))
                }
            }
        }
    }
);

/// Add the number accessors to NSString.
pub(crate) fn install(target: &AnyClass) {
    super::install::copy_methods(Helper::class(), target, false);
}
