//! `NSScanner`.
//!
//! A scanner walks an immutable copy of its string by UTF-16 location.
//! Every scan first skips `charactersToBeSkipped` (whitespace and newlines
//! unless changed), then reads; a scan that fails leaves the location where
//! it was. Each scan reads only as far as what it scans, so a loop of scans
//! over long text costs what the text does. Decimal numbers take digits of
//! any script; integers may have skipped characters between their sign and
//! digits; numbers saturate on overflow and still consume all their digits,
//! as on macOS. The `intoString:` out-parameters are written autoreleased,
//! as Cocoa's convention for out-parameters asks.

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSCharacterSet, NSInteger, NSString, NSUInteger, NSZone};

use crate::charset;
use crate::string::fold::{ANCHORED, CASE_INSENSITIVE, LITERAL};
use crate::string::index::Text;
use crate::string::view::view;
use crate::string::{numeric, search, wtf8};

sidestep_runtime::static_class!(pub(crate) NSSCANNER, NSSCANNER_META = "NSScanner", || {
    let _ = NSScannerImpl::class();
});

pub(crate) struct ScannerIvars {
    string: Retained<NSString>,
    location: Cell<usize>,
    skip: RefCell<Option<Retained<NSCharacterSet>>>,
    case_sensitive: Cell<bool>,
    locale: RefCell<Option<Retained<AnyObject>>>,
}

/// The digits of a hexadecimal number.
fn hex_digit(c: u8) -> Option<u64> {
    (c as char).to_digit(16).map(u64::from)
}

impl NSScannerImpl {
    /// The text and the location after skipping.
    fn with_text<R>(&self, f: impl FnOnce(&Text, usize) -> R) -> R {
        let iv = self.ivars();
        let v = view(&iv.string);
        let t = v.text();
        let mut at = iv.location.get().min(t.utf16_len);
        self.skipping(|skip| {
            if let Some(m) = skip {
                at = t.utf16_at(skip_members(t.bytes, t.pos(at).byte, m));
            }
        });
        f(&t, at)
    }

    /// Run a scan: `f` gets the text and the start after skipping and
    /// returns the end of what it read, or `None` to leave the location.
    fn scan(&self, f: impl FnOnce(&Text, usize) -> Option<usize>) -> bool {
        let end = self.with_text(f);
        if let Some(end) = end {
            self.ivars().location.set(end);
        }
        end.is_some()
    }

    /// The ASCII bytes at the scan position (numbers are ASCII), with the
    /// UTF-16 start.
    fn scan_bytes<T>(&self, f: impl FnOnce(&[u8]) -> Option<(T, usize)>) -> Option<T> {
        let mut out = None;
        self.scan(|t, at| {
            let byte = t.pos(at).byte;
            let (value, used) = f(&t.bytes[byte..])?;
            out = Some(value);
            Some(t.utf16_at(byte + used))
        });
        out
    }

    /// The membership test of the skip set, if there is one.
    fn skipping<R>(&self, f: impl FnOnce(Option<&charset::Membership>) -> R) -> R {
        // A set of another class answers by message, so no borrow is kept.
        let skip = self.ivars().skip.borrow().clone();
        let m = skip.as_deref().map(|set| charset::membership(set));
        f(m.as_ref())
    }

    /// A decimal integer, saturated to `min..=max`: a sign (a minus only
    /// when `min` is below zero), skipped characters after it, then digits
    /// of any script, all of which are consumed.
    fn scan_integer(&self, min: i128, max: i128) -> Option<i128> {
        let mut out = None;
        self.skipping(|skip| {
            self.scan(|t, at| {
                let b = t.bytes;
                let begin = t.pos(at).byte;
                let mut i = begin;
                let negative = match b.get(i) {
                    Some(b'-') if min < 0 => {
                        i += 1;
                        true
                    }
                    Some(b'-') => return None,
                    Some(b'+') => {
                        i += 1;
                        false
                    }
                    _ => false,
                };
                if i > begin
                    && let Some(m) = skip
                {
                    i = skip_members(b, i, m);
                }
                let mut value: i128 = 0;
                let mut any = false;
                while i < b.len() {
                    let (c, w) = wtf8::decode(b, i);
                    let Some(d) = numeric::digit(c) else { break };
                    value = (value * 10 + i128::from(d)).min(i128::from(u64::MAX) + 1);
                    any = true;
                    i += w;
                }
                if !any {
                    return None;
                }
                out = Some((if negative { -value } else { value }).clamp(min, max));
                Some(t.utf16_at(i))
            })
        });
        out
    }

    fn scan_hex(&self, max: u64) -> Option<u64> {
        self.scan_bytes(|b| {
            let mut i = 0;
            if b.len() >= 3 && b[0] == b'0' && (b[1] == b'x' || b[1] == b'X') && hex_digit(b[2]).is_some() {
                i = 2;
            }
            let start = i;
            let mut value: u64 = 0;
            let mut overflow = false;
            while let Some(d) = b.get(i).and_then(|&c| hex_digit(c)) {
                match value.checked_mul(16).and_then(|v| v.checked_add(d)) {
                    Some(v) if v <= max => value = v,
                    _ => overflow = true,
                }
                i += 1;
            }
            (i > start).then_some((if overflow { max } else { value }, i))
        })
    }

    /// A decimal floating-point number: a sign, digits of any script with
    /// an optional fraction, and an exponent if digits follow its `e`.
    fn scan_double(&self) -> Option<f64> {
        let mut out = None;
        self.scan(|t, at| {
            let b = t.bytes;
            let start = t.pos(at).byte;
            let mut i = start;
            let mut number = String::new();
            if let Some(&c) = b.get(i).filter(|&&c| c == b'+' || c == b'-') {
                number.push(char::from(c));
                i += 1;
            }
            let digits = |i: &mut usize, number: &mut String| {
                let mut n = 0;
                while *i < b.len() {
                    let (c, w) = wtf8::decode(b, *i);
                    let Some(d) = numeric::digit(c) else { break };
                    number.push(char::from_digit(d, 10).expect("a digit"));
                    *i += w;
                    n += 1;
                }
                n
            };
            let mut n = digits(&mut i, &mut number);
            if b.get(i) == Some(&b'.') {
                i += 1;
                number.push('.');
                n += digits(&mut i, &mut number);
            }
            if n == 0 {
                return None;
            }
            if matches!(b.get(i), Some(b'e' | b'E')) {
                let mut j = i + 1;
                let mut exponent = String::from("e");
                if let Some(&c) = b.get(j).filter(|&&c| c == b'+' || c == b'-') {
                    exponent.push(char::from(c));
                    j += 1;
                }
                if digits(&mut j, &mut exponent) > 0 {
                    number.push_str(&exponent);
                    i = j;
                }
            }
            out = Some(number.parse::<f64>().ok()?);
            Some(t.utf16_at(i))
        });
        out
    }

    /// A hexadecimal floating-point number: a sign, then `0x` and at least
    /// one hex digit, an optional fraction, and a binary exponent if digits
    /// follow its `p`.
    fn scan_hex_double(&self) -> Option<f64> {
        self.scan_bytes(|b| {
            let mut i = 0;
            let negative = match b.first() {
                Some(b'-') => {
                    i = 1;
                    true
                }
                Some(b'+') => {
                    i = 1;
                    false
                }
                _ => false,
            };
            if !(b.get(i) == Some(&b'0') && matches!(b.get(i + 1), Some(b'x' | b'X'))) {
                return None;
            }
            i += 2;
            let (mut mantissa, mut exp, mut digits) = (0f64, 0i32, 0);
            while let Some(d) = b.get(i).and_then(|&c| hex_digit(c)) {
                mantissa = mantissa * 16.0 + d as f64;
                digits += 1;
                i += 1;
            }
            if digits == 0 {
                return None;
            }
            if b.get(i) == Some(&b'.') {
                i += 1;
                while let Some(d) = b.get(i).and_then(|&c| hex_digit(c)) {
                    mantissa = mantissa * 16.0 + d as f64;
                    exp -= 4;
                    i += 1;
                }
            }
            if matches!(b.get(i), Some(b'p' | b'P')) {
                let mut j = i + 1;
                let sign = match b.get(j) {
                    Some(b'-') => {
                        j += 1;
                        -1
                    }
                    Some(b'+') => {
                        j += 1;
                        1
                    }
                    _ => 1,
                };
                let start = j;
                let mut p = 0i32;
                while let Some(d) = b.get(j).filter(|c| c.is_ascii_digit()) {
                    p = p.saturating_mul(10).saturating_add(i32::from(d - b'0'));
                    j += 1;
                }
                if j > start {
                    exp = exp.saturating_add(sign * p);
                    i = j;
                }
            }
            let v = mantissa * 2f64.powi(exp);
            Some((if negative { -v } else { v }, i))
        })
    }

    /// Scan text matching `string`, or up to it.
    fn scan_string(&self, string: &NSString, up_to: bool, out: *mut *mut NSString) -> bool {
        let options = if self.ivars().case_sensitive.get() { LITERAL } else { CASE_INSENSITIVE };
        let nv = view(string);
        let n = nv.text();
        let mut range = None;
        let ok = self.scan(|t, at| {
            let rest = t.utf16_len - at;
            let end = if up_to {
                match search::find(t, at, rest, &n, options) {
                    Some((loc, _)) if loc == at => return None,
                    Some((loc, _)) => loc,
                    None if rest > 0 => t.utf16_len,
                    None => return None,
                }
            } else {
                let (loc, len) = search::find(t, at, rest, &n, options | ANCHORED)?;
                loc + len
            };
            range = Some((at, end));
            Some(end)
        });
        self.write(out, range);
        ok
    }

    /// Scan characters in (or not in) a set.
    fn scan_set(&self, set: &NSCharacterSet, up_to: bool, out: *mut *mut NSString) -> bool {
        let mut range = None;
        let ok = self.scan(|t, at| {
            let m = charset::membership(set);
            let mut byte = t.pos(at).byte;
            let start = byte;
            while byte < t.bytes.len() {
                let (c, w) = wtf8::decode(t.bytes, byte);
                if m.contains(c) == up_to {
                    break;
                }
                byte += w;
            }
            if byte == start {
                return None;
            }
            let end = t.utf16_at(byte);
            range = Some((at, end));
            Some(end)
        });
        self.write(out, range);
        ok
    }

    /// Write the scanned text, autoreleased, to an out-parameter.
    fn write(&self, out: *mut *mut NSString, range: Option<(usize, usize)>) {
        if out.is_null() {
            return;
        }
        if let Some((s, e)) = range {
            let r = objc2_foundation::NSRange::new(s, e - s);
            let sub = self.ivars().string.substringWithRange(r);
            // SAFETY: the caller passes a valid out-parameter; the string
            // is handed over autoreleased.
            unsafe { *out = Retained::autorelease_ptr(sub) };
        }
    }
}

/// The byte after the members of `m` from byte `at` of `b`.
fn skip_members(b: &[u8], mut at: usize, m: &charset::Membership) -> usize {
    while at < b.len() {
        let (c, w) = wtf8::decode(b, at);
        if !m.contains(c) {
            break;
        }
        at += w;
    }
    at
}

/// A new scanner over a copy of `string`, skipping whitespace and newlines.
fn start(this: Allocated<NSScannerImpl>, string: &NSString) -> Retained<NSScannerImpl> {
    // SAFETY: -copy of a string is an immutable string.
    let string: Retained<NSString> = unsafe { msg_send![string, copy] };
    let this = this.set_ivars(ScannerIvars {
        string,
        location: Cell::new(0),
        skip: RefCell::new(Some(NSCharacterSet::whitespaceAndNewlineCharacterSet())),
        case_sensitive: Cell::new(false),
        locale: RefCell::new(None),
    });
    // SAFETY: NSObject's initializer.
    unsafe { msg_send![super(this), init] }
}

fn write_value<T>(ptr: *mut T, value: Option<T>) -> bool {
    match value {
        Some(v) => {
            if !ptr.is_null() {
                // SAFETY: the caller passes a valid pointer or null.
                unsafe { ptr.write(v) };
            }
            true
        }
        None => false,
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSScanner"]
    #[ivars = ScannerIvars]
    pub(crate) struct NSScannerImpl;

    impl NSScannerImpl {
        #[unsafe(method_id(initWithString:))]
        fn init_with_string(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            start(this, string)
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            start(this, &crate::string::empty())
        }

        #[unsafe(method_id(scannerWithString:))]
        fn scanner_with_string(string: &NSString) -> Retained<Self> {
            start(Self::alloc(), string)
        }

        #[unsafe(method_id(localizedScannerWithString:))]
        fn localized_scanner_with_string(string: &NSString) -> Retained<AnyObject> {
            Retained::into_super(Retained::into_super(start(Self::alloc(), string)))
        }

        #[unsafe(method_id(string))]
        fn string(&self) -> Retained<NSString> {
            self.ivars().string.clone()
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            let iv = self.ivars();
            let this = Self::alloc().set_ivars(ScannerIvars {
                string: iv.string.clone(),
                location: Cell::new(iv.location.get()),
                skip: RefCell::new(iv.skip.borrow().clone()),
                case_sensitive: Cell::new(iv.case_sensitive.get()),
                locale: RefCell::new(iv.locale.borrow().clone()),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(scanLocation))]
        fn scan_location(&self) -> NSUInteger {
            self.ivars().location.get()
        }

        #[unsafe(method(setScanLocation:))]
        fn set_scan_location(&self, location: NSUInteger) {
            let len = self.ivars().string.length();
            if location > len {
                panic!("-[NSScanner setScanLocation:]: Index {location} out of bounds; string length {len}");
            }
            self.ivars().location.set(location);
        }

        #[unsafe(method_id(charactersToBeSkipped))]
        fn characters_to_be_skipped(&self) -> Option<Retained<NSCharacterSet>> {
            self.ivars().skip.borrow().clone()
        }

        #[unsafe(method(setCharactersToBeSkipped:))]
        fn set_characters_to_be_skipped(&self, set: Option<&NSCharacterSet>) {
            // SAFETY: -copy of a character set is an immutable set.
            let set = set.map(|s| unsafe { msg_send![s, copy] });
            *self.ivars().skip.borrow_mut() = set;
        }

        #[unsafe(method(caseSensitive))]
        fn case_sensitive(&self) -> bool {
            self.ivars().case_sensitive.get()
        }

        #[unsafe(method(setCaseSensitive:))]
        fn set_case_sensitive(&self, case_sensitive: bool) {
            self.ivars().case_sensitive.set(case_sensitive);
        }

        #[unsafe(method_id(locale))]
        fn locale(&self) -> Option<Retained<AnyObject>> {
            self.ivars().locale.borrow().clone()
        }

        #[unsafe(method(setLocale:))]
        fn set_locale(&self, locale: Option<&AnyObject>) {
            *self.ivars().locale.borrow_mut() = locale.map(|l| l.retain());
        }

        #[unsafe(method(isAtEnd))]
        fn is_at_end(&self) -> bool {
            self.with_text(|t, at| at >= t.utf16_len)
        }

        #[unsafe(method(scanInt:))]
        fn scan_int(&self, result: *mut i32) -> bool {
            write_value(result, self.scan_integer(i32::MIN.into(), i32::MAX.into()).map(|v| v as i32))
        }

        #[unsafe(method(scanInteger:))]
        fn scan_integer_value(&self, result: *mut NSInteger) -> bool {
            let v = self.scan_integer(NSInteger::MIN as i128, NSInteger::MAX as i128);
            write_value(result, v.map(|v| v as NSInteger))
        }

        #[unsafe(method(scanLongLong:))]
        fn scan_long_long(&self, result: *mut i64) -> bool {
            write_value(result, self.scan_integer(i64::MIN.into(), i64::MAX.into()).map(|v| v as i64))
        }

        #[unsafe(method(scanUnsignedLongLong:))]
        fn scan_unsigned_long_long(&self, result: *mut u64) -> bool {
            write_value(result, self.scan_integer(0, u64::MAX.into()).map(|v| v as u64))
        }

        #[unsafe(method(scanFloat:))]
        fn scan_float(&self, result: *mut f32) -> bool {
            write_value(result, self.scan_double().map(|v| v as f32))
        }

        #[unsafe(method(scanDouble:))]
        fn scan_double_value(&self, result: *mut f64) -> bool {
            write_value(result, self.scan_double())
        }

        #[unsafe(method(scanHexInt:))]
        fn scan_hex_int(&self, result: *mut u32) -> bool {
            write_value(result, self.scan_hex(u32::MAX.into()).map(|v| v as u32))
        }

        #[unsafe(method(scanHexLongLong:))]
        fn scan_hex_long_long(&self, result: *mut u64) -> bool {
            write_value(result, self.scan_hex(u64::MAX))
        }

        #[unsafe(method(scanHexFloat:))]
        fn scan_hex_float(&self, result: *mut f32) -> bool {
            write_value(result, self.scan_hex_double().map(|v| v as f32))
        }

        #[unsafe(method(scanHexDouble:))]
        fn scan_hex_double_value(&self, result: *mut f64) -> bool {
            write_value(result, self.scan_hex_double())
        }

        #[unsafe(method(scanString:intoString:))]
        fn scan_string_into(&self, string: &NSString, out: *mut *mut NSString) -> bool {
            self.scan_string(string, false, out)
        }

        #[unsafe(method(scanUpToString:intoString:))]
        fn scan_up_to_string_into(&self, string: &NSString, out: *mut *mut NSString) -> bool {
            self.scan_string(string, true, out)
        }

        #[unsafe(method(scanCharactersFromSet:intoString:))]
        fn scan_characters_into(&self, set: &NSCharacterSet, out: *mut *mut NSString) -> bool {
            self.scan_set(set, false, out)
        }

        #[unsafe(method(scanUpToCharactersFromSet:intoString:))]
        fn scan_up_to_characters_into(&self, set: &NSCharacterSet, out: *mut *mut NSString) -> bool {
            self.scan_set(set, true, out)
        }
    }

    unsafe impl NSObjectProtocol for NSScannerImpl {}
);
