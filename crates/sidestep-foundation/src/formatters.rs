//! Number, byte-count and relative date formatters used by Cocoa controls.
use crate::thread::lock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSDate, NSFormatter, NSLocale, NSNumber, NSString, NSZone};
use std::sync::Mutex;
sidestep_runtime::static_class!(pub(crate) NUMBER, NUMBER_META = "NSNumberFormatter", || { let _ = Number::class(); });
sidestep_runtime::static_class!(pub(crate) BYTES, BYTES_META = "NSByteCountFormatter", || { let _ = Bytes::class(); });
sidestep_runtime::static_class!(pub(crate) RELATIVE, RELATIVE_META = "NSRelativeDateTimeFormatter", || { let _ = Relative::class(); });

#[derive(Clone)]
struct NumberState {
    style: usize,
    grouping: bool,
    floats: bool,
    min_fraction: usize,
    max_fraction: usize,
    min_integer: usize,
    min: Option<Retained<NSNumber>>,
    max: Option<Retained<NSNumber>>,
    locale: Retained<NSLocale>,
}
impl Default for NumberState {
    fn default() -> Self {
        Self {
            style: 0,
            grouping: false,
            floats: true,
            min_fraction: 0,
            max_fraction: 0,
            min_integer: 1,
            min: None,
            max: None,
            locale: NSLocale::currentLocale(),
        }
    }
}
define_class!(
    #[unsafe(super(NSFormatter, NSObject))]
    #[name = "NSNumberFormatter"]
    #[ivars = Mutex<NumberState>]
    struct Number;
    impl Number {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> { let this = this.set_ivars(Mutex::new(NumberState::default())); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(copyWithZone:))]
        fn copy(&self, _zone: *mut NSZone) -> Retained<Self> { let this = Self::alloc().set_ivars(Mutex::new(lock(self.ivars()).clone())); unsafe { msg_send![super(this), init] } }
        #[unsafe(method(numberStyle))]
        fn style(&self) -> usize { lock(self.ivars()).style }
        #[unsafe(method(setNumberStyle:))]
        fn set_style(&self, value: usize) { let mut s = lock(self.ivars()); s.style = value; s.grouping = value != 0; s.min_fraction = if value == 2 { 2 } else { 0 }; s.max_fraction = match value { 1 => 3, 2 => 2, _ => 0 }; }
        #[unsafe(method(usesGroupingSeparator))]
        fn grouping(&self) -> bool { lock(self.ivars()).grouping }
        #[unsafe(method(setUsesGroupingSeparator:))]
        fn set_grouping(&self, value: bool) { lock(self.ivars()).grouping = value; }
        #[unsafe(method(allowsFloats))]
        fn floats(&self) -> bool { lock(self.ivars()).floats }
        #[unsafe(method(setAllowsFloats:))]
        fn set_floats(&self, value: bool) { lock(self.ivars()).floats = value; }
        #[unsafe(method(minimumFractionDigits))]
        fn min_fraction(&self) -> usize { lock(self.ivars()).min_fraction }
        #[unsafe(method(setMinimumFractionDigits:))]
        fn set_min_fraction(&self, value: usize) { lock(self.ivars()).min_fraction = value; }
        #[unsafe(method(maximumFractionDigits))]
        fn max_fraction(&self) -> usize { lock(self.ivars()).max_fraction }
        #[unsafe(method(setMaximumFractionDigits:))]
        fn set_max_fraction(&self, value: usize) { lock(self.ivars()).max_fraction = value; }
        #[unsafe(method(minimumIntegerDigits))]
        fn min_integer(&self) -> usize { lock(self.ivars()).min_integer }
        #[unsafe(method(setMinimumIntegerDigits:))]
        fn set_min_integer(&self, value: usize) { lock(self.ivars()).min_integer = value; }
        #[unsafe(method_id(minimum))]
        fn min(&self) -> Option<Retained<NSNumber>> { lock(self.ivars()).min.clone() }
        #[unsafe(method(setMinimum:))]
        fn set_min(&self, value: Option<&NSNumber>) { lock(self.ivars()).min = value.map(|v| v.retain()); }
        #[unsafe(method_id(maximum))]
        fn max(&self) -> Option<Retained<NSNumber>> { lock(self.ivars()).max.clone() }
        #[unsafe(method(setMaximum:))]
        fn set_max(&self, value: Option<&NSNumber>) { lock(self.ivars()).max = value.map(|v| v.retain()); }
        #[unsafe(method_id(locale))]
        fn locale(&self) -> Retained<NSLocale> { lock(self.ivars()).locale.clone() }
        #[unsafe(method(setLocale:))]
        fn set_locale(&self, value: Option<&NSLocale>) { lock(self.ivars()).locale = value.map_or_else(NSLocale::currentLocale, Message::retain); }
        #[unsafe(method_id(stringFromNumber:))]
        fn string(&self, number: &NSNumber) -> Option<Retained<NSString>> { number_string(&lock(self.ivars()), number.doubleValue()).map(|s| NSString::from_str(&s)) }
        #[unsafe(method_id(stringForObjectValue:))]
        fn string_object(&self, value: Option<&AnyObject>) -> Option<Retained<NSString>> { value.and_then(|v| v.downcast_ref::<NSNumber>()).and_then(|n| number_string(&lock(self.ivars()), n.doubleValue())).map(|s| NSString::from_str(&s)) }
        #[unsafe(method_id(numberFromString:))]
        fn number(&self, text: &NSString) -> Option<Retained<NSNumber>> { parse_number(&lock(self.ivars()), &text.to_string()).map(NSNumber::numberWithDouble) }
        #[unsafe(method(getObjectValue:forString:errorDescription:))]
        fn parse(&self, out: *mut *mut AnyObject, text: &NSString, error: *mut *mut NSString) -> bool {
            match parse_number(&lock(self.ivars()), &text.to_string()) {
                Some(value) => { if !out.is_null() { unsafe { out.write(Retained::autorelease_ptr(NSNumber::numberWithDouble(value).into())) }; } true },
                None => { if !error.is_null() { unsafe { error.write(Retained::autorelease_ptr(NSString::from_str("The value is not a valid number."))) }; } false }
            }
        }
    }
    unsafe impl NSObjectProtocol for Number {}
);
fn number_string(s: &NumberState, value: f64) -> Option<String> {
    let value = if s.style == 3 { value * 100.0 } else { value };
    if value.is_nan() {
        return Some("NaN".into());
    }
    if value.is_infinite() {
        return Some(if value.is_sign_negative() { "-∞" } else { "+∞" }.into());
    }
    if !matches!(s.style, 0..=4) {
        return None;
    }
    if s.style == 4 {
        return Some(format!("{value:E}"));
    }
    let max = s.max_fraction.max(s.min_fraction).min(100);
    let mut text = format!("{:.*}", max, value.abs());
    if let Some(dot) = text.find('.') {
        while text.ends_with('0') && text.len() - dot - 1 > s.min_fraction {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    let (integer, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let integer = format!("{:0>width$}", integer, width = s.min_integer.min(100));
    let decimal = s.locale.decimalSeparator().to_string();
    let group = s.locale.groupingSeparator().to_string();
    let mut out = String::new();
    if value.is_sign_negative() {
        out.push('-');
    }
    if s.style == 2 {
        out.push_str(&s.locale.currencySymbol().to_string());
    }
    for (i, ch) in integer.chars().enumerate() {
        if s.grouping && i > 0 && (integer.len() - i).is_multiple_of(3) {
            out.push_str(&group);
        }
        out.push(ch);
    }
    if !fraction.is_empty() {
        out.push_str(&decimal);
        out.push_str(fraction);
    }
    if s.style == 3 {
        out.push('%');
    }
    Some(out)
}
fn parse_number(s: &NumberState, text: &str) -> Option<f64> {
    let mut text = text.trim().to_string();
    if s.style == 2 {
        text = text.replace(&s.locale.currencySymbol().to_string(), "");
    }
    if s.grouping {
        text = text.replace(&s.locale.groupingSeparator().to_string(), "");
    }
    text = text.replace(&s.locale.decimalSeparator().to_string(), ".");
    if s.style == 3 {
        text = text.strip_suffix('%')?.to_string();
    }
    let value = text.parse::<f64>().ok()? / if s.style == 3 { 100.0 } else { 1.0 };
    if !value.is_finite()
        || (!s.floats && value.fract() != 0.0)
        || s.min.as_ref().is_some_and(|n| value < n.doubleValue())
        || s.max.as_ref().is_some_and(|n| value > n.doubleValue())
    {
        None
    } else {
        Some(value)
    }
}
#[derive(Clone)]
struct ByteState {
    style: isize,
    units: usize,
    adaptive: bool,
    count: bool,
    unit: bool,
    nonnumeric: bool,
    actual: bool,
}
impl Default for ByteState {
    fn default() -> Self {
        Self { style: 0, units: 0, adaptive: true, count: true, unit: true, nonnumeric: true, actual: false }
    }
}
define_class!(
    #[unsafe(super(NSFormatter, NSObject))]
    #[name = "NSByteCountFormatter"]
    #[ivars = Mutex<ByteState>]
    struct Bytes;
    impl Bytes {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> { let this = this.set_ivars(Mutex::new(ByteState::default())); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(stringFromByteCount:countStyle:))]
        fn string_class(value: i64, style: isize) -> Retained<NSString> { NSString::from_str(&byte_string(&ByteState { style, ..Default::default() }, value)) }
        #[unsafe(method_id(stringFromByteCount:))]
        fn string(&self, value: i64) -> Retained<NSString> { NSString::from_str(&byte_string(&lock(self.ivars()), value)) }
        #[unsafe(method_id(stringForObjectValue:))]
        fn string_object(&self, value: Option<&AnyObject>) -> Option<Retained<NSString>> { value.and_then(|v| v.downcast_ref::<NSNumber>()).map(|n| NSString::from_str(&byte_string(&lock(self.ivars()), n.longLongValue()))) }
        #[unsafe(method(countStyle))]
        fn style(&self) -> isize { lock(self.ivars()).style }
        #[unsafe(method(setCountStyle:))]
        fn set_style(&self, value: isize) { lock(self.ivars()).style = value; }
        #[unsafe(method(allowedUnits))]
        fn units(&self) -> usize { lock(self.ivars()).units }
        #[unsafe(method(setAllowedUnits:))]
        fn set_units(&self, value: usize) { lock(self.ivars()).units = value; }
        #[unsafe(method(isAdaptive))]
        fn adaptive(&self) -> bool { lock(self.ivars()).adaptive }
        #[unsafe(method(setAdaptive:))]
        fn set_adaptive(&self, value: bool) { lock(self.ivars()).adaptive = value; }
        #[unsafe(method(includesCount))]
        fn count(&self) -> bool { lock(self.ivars()).count }
        #[unsafe(method(setIncludesCount:))]
        fn set_count(&self, value: bool) { lock(self.ivars()).count = value; }
        #[unsafe(method(includesUnit))]
        fn unit(&self) -> bool { lock(self.ivars()).unit }
        #[unsafe(method(setIncludesUnit:))]
        fn set_unit(&self, value: bool) { lock(self.ivars()).unit = value; }
        #[unsafe(method(allowsNonnumericFormatting))]
        fn nonnumeric(&self) -> bool { lock(self.ivars()).nonnumeric }
        #[unsafe(method(setAllowsNonnumericFormatting:))]
        fn set_nonnumeric(&self, value: bool) { lock(self.ivars()).nonnumeric = value; }
        #[unsafe(method(includesActualByteCount))]
        fn actual(&self) -> bool { lock(self.ivars()).actual }
        #[unsafe(method(setIncludesActualByteCount:))]
        fn set_actual(&self, value: bool) { lock(self.ivars()).actual = value; }
    }
    unsafe impl NSObjectProtocol for Bytes {}
);
fn byte_string(s: &ByteState, value: i64) -> String {
    if value == 0 && s.nonnumeric && s.unit && s.count {
        return "Zero KB".into();
    }
    let base: f64 = if matches!(s.style, 1 | 3) { 1024.0 } else { 1000.0 };
    let mut n = value.unsigned_abs() as f64;
    let mut index = 0usize;
    while n >= base && index < 8 {
        n /= base;
        index += 1;
    }
    if s.units != 0 {
        index = (0usize..=8).filter(|i| s.units & (1 << i) != 0).min_by_key(|i| i.abs_diff(index)).unwrap_or(index);
        n = value.unsigned_abs() as f64 / base.powi(index as i32);
    }
    if value < 0 {
        n = -n;
    }
    let digits = if s.adaptive {
        match index {
            0 | 1 => 0,
            2 => 1,
            _ => 2,
        }
    } else {
        2
    };
    let mut count = format!("{n:.digits$}");
    if count.contains('.') {
        while count.ends_with('0') {
            count.pop();
        }
        if count.ends_with('.') {
            count.pop();
        }
    }
    let unit =
        [if value.unsigned_abs() == 1 { "byte" } else { "bytes" }, "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"]
            [index];
    let mut out = match (s.count, s.unit) {
        (true, true) => format!("{count} {unit}"),
        (true, false) => count,
        (false, true) => unit.into(),
        _ => String::new(),
    };
    if s.actual && index > 0 {
        out.push_str(&format!(" ({value} bytes)"));
    }
    out
}
#[derive(Clone, Default)]
struct RelativeState {
    units: isize,
    style: isize,
    context: isize,
}
define_class!(
    #[unsafe(super(NSFormatter, NSObject))]
    #[name = "NSRelativeDateTimeFormatter"]
    #[ivars = Mutex<RelativeState>]
    struct Relative;
    impl Relative {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> { let this = this.set_ivars(Mutex::new(RelativeState::default())); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(localizedStringForDate:relativeToDate:))]
        fn date(&self, date: &NSDate, relative: &NSDate) -> Retained<NSString> { NSString::from_str(&relative_string(&lock(self.ivars()), crate::date::time_of(date) - crate::date::time_of(relative))) }
        #[unsafe(method_id(localizedStringFromTimeInterval:))]
        fn interval(&self, seconds: f64) -> Retained<NSString> { NSString::from_str(&relative_string(&lock(self.ivars()), seconds)) }
        #[unsafe(method(unitsStyle))]
        fn units(&self) -> isize { lock(self.ivars()).units }
        #[unsafe(method(setUnitsStyle:))]
        fn set_units(&self, value: isize) { lock(self.ivars()).units = value; }
        #[unsafe(method(dateTimeStyle))]
        fn style(&self) -> isize { lock(self.ivars()).style }
        #[unsafe(method(setDateTimeStyle:))]
        fn set_style(&self, value: isize) { lock(self.ivars()).style = value; }
        #[unsafe(method(formattingContext))]
        fn context(&self) -> isize { lock(self.ivars()).context }
        #[unsafe(method(setFormattingContext:))]
        fn set_context(&self, value: isize) { lock(self.ivars()).context = value; }
    }
    unsafe impl NSObjectProtocol for Relative {}
);
fn relative_string(s: &RelativeState, seconds: f64) -> String {
    let abs = seconds.abs();
    let (unit, size) = if abs < 60.0 {
        ("second", 1.0)
    } else if abs < 3600.0 {
        ("minute", 60.0)
    } else if abs < 86400.0 {
        ("hour", 3600.0)
    } else if abs < 604800.0 {
        ("day", 86400.0)
    } else if abs < 2629800.0 {
        ("week", 604800.0)
    } else if abs < 31557600.0 {
        ("month", 2629800.0)
    } else {
        ("year", 31557600.0)
    };
    let n = (abs / size).round() as i64;
    if s.style == 1 {
        if n == 0 {
            return "now".into();
        }
        if n == 1 {
            if unit == "day" {
                return if seconds < 0.0 { "yesterday" } else { "tomorrow" }.into();
            }
            return format!("{} {unit}", if seconds < 0.0 { "last" } else { "next" });
        }
    }
    let label = if matches!(s.units, 2 | 3) {
        match unit {
            "second" => "sec.",
            "minute" => "min.",
            "hour" => "hr.",
            "week" => "wk.",
            "month" => "mo.",
            "year" => "yr.",
            _ => unit,
        }
    } else {
        unit
    };
    let plural = if n != 1 && label == unit { "s" } else { "" };
    if seconds < 0.0 { format!("{n} {label}{plural} ago") } else { format!("in {n} {label}{plural}") }
}
