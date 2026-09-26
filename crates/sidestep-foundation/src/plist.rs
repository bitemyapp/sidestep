//! Property lists: `NSPropertyListSerialization`, and the conversions
//! between Foundation objects and `plist::Value` that `NSBundle` and
//! `NSUserDefaults` use.
//!
//! Reading takes XML, binary and OpenStep text through the `plist` crate.
//! Binary output comes from the crate too; XML output is written here, in
//! exactly the layout macOS writes (tab indentation, sorted dictionary
//! keys, `%.17g` reals, `<dict/>` for an empty dictionary, base64 in
//! 76-character lines), which `conformance/tests/defaults.rs` pins byte
//! for byte.
//!
//! An `NSNumber` whose C type is `c` and whose value is 0 or 1 is a
//! boolean, as GNUstep also has it.

use std::fmt::Write as _;
use std::io::Cursor;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{ClassType, define_class, msg_send};
use objc2_foundation::{NSData, NSDate, NSDictionary, NSError, NSString, NSUInteger};
use plist::Value;

sidestep_runtime::static_class!(
    pub(crate) NSPROPERTYLISTSERIALIZATION,
    NSPROPERTYLISTSERIALIZATION_META = "NSPropertyListSerialization",
    || {
        let _ = NSPropertyListSerializationImpl::class();
        crate::perform::install();
    }
);

/// `NSPropertyListFormat`.
pub(crate) const OPENSTEP: NSUInteger = 1;
pub(crate) const XML: NSUInteger = 100;
pub(crate) const BINARY: NSUInteger = 200;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSPropertyListSerialization"]
    pub(crate) struct NSPropertyListSerializationImpl;

    impl NSPropertyListSerializationImpl {
        #[unsafe(method_id(propertyListWithData:options:format:error:))]
        fn property_list_with_data(
            data: &NSData,
            _options: NSUInteger,
            format: *mut NSUInteger,
            error: *mut *mut NSError,
        ) -> Option<Retained<AnyObject>> {
            // SAFETY: the caller passes null or room for a format and an
            // error; nothing mutates the data while it is read.
            unsafe { read_object(crate::data::bytes(data), format, error) }
        }

        #[unsafe(method_id(dataWithPropertyList:format:options:error:))]
        fn data_with_property_list(
            list: &AnyObject,
            format: NSUInteger,
            _options: NSUInteger,
            error: *mut *mut NSError,
        ) -> Option<Retained<NSData>> {
            // SAFETY: the caller passes null or room for an error.
            unsafe { write_object(list, format, error) }
        }

        #[unsafe(method(propertyList:isValidForFormat:))]
        fn is_valid(list: &AnyObject, format: NSUInteger) -> bool {
            matches!(format, XML | BINARY) && from_object(list).is_some()
        }
    }
);

/// # Safety
///
/// `format` and `error` are null or writable.
unsafe fn read_object(bytes: &[u8], format: *mut NSUInteger, error: *mut *mut NSError) -> Option<Retained<AnyObject>> {
    let object = parse(bytes).and_then(|(value, found)| Some((to_object(&value)?, found)));
    match object {
        Some((object, found)) => {
            if !format.is_null() {
                // SAFETY: per this function's contract.
                unsafe { format.write(found) };
            }
            Some(object)
        }
        None => {
            // SAFETY: per this function's contract.
            unsafe {
                crate::error::set(error, crate::error::cocoa(crate::error::code::PROPERTY_LIST_READ_CORRUPT, &[]))
            };
            None
        }
    }
}

/// # Safety
///
/// `error` is null or writable.
unsafe fn write_object(list: &AnyObject, format: NSUInteger, error: *mut *mut NSError) -> Option<Retained<NSData>> {
    let bytes = from_object(list).and_then(|value| match format {
        XML => Some(write_xml(&value)),
        BINARY => write_binary(&value),
        _ => None,
    });
    match bytes {
        Some(bytes) => Some(NSData::from_vec(bytes)),
        None => {
            // SAFETY: per this function's contract.
            unsafe {
                crate::error::set(error, crate::error::cocoa(crate::error::code::PROPERTY_LIST_WRITE_INVALID, &[]))
            };
            None
        }
    }
}

/// Parse a property list in any format, returning it and its format.
pub(crate) fn parse(bytes: &[u8]) -> Option<(Value, NSUInteger)> {
    if bytes.starts_with(b"bplist") {
        return Value::from_reader(Cursor::new(bytes)).ok().map(|v| (v, BINARY));
    }
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let start = text.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(text.len());
    if text[start..].starts_with(b"<") {
        Value::from_reader_xml(Cursor::new(text)).ok().map(|v| (v, XML))
    } else {
        Value::from_reader_ascii(Cursor::new(text)).ok().map(|v| (v, OPENSTEP))
    }
}

/// A binary property list.
pub(crate) fn write_binary(value: &Value) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    value.to_writer_binary(&mut out).ok()?;
    Some(out)
}

const XML_HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n";

/// An XML property list, laid out as macOS lays it out.
pub(crate) fn write_xml(value: &Value) -> Vec<u8> {
    let mut out = String::from(XML_HEADER);
    write_value(&mut out, value, 0);
    out.push_str("</plist>\n");
    out.into_bytes()
}

/// [`write_xml`] of a dictionary, without making a `Value` of it.
pub(crate) fn write_xml_dictionary(dict: &plist::Dictionary) -> Vec<u8> {
    let mut out = String::from(XML_HEADER);
    indent(&mut out, 0);
    write_dictionary(&mut out, dict, 0);
    out.push_str("</plist>\n");
    out.into_bytes()
}

/// A dictionary's elements, the opening tag's indent already written.
fn write_dictionary(out: &mut String, dict: &plist::Dictionary, depth: usize) {
    if dict.is_empty() {
        out.push_str("<dict/>\n");
        return;
    }
    out.push_str("<dict>\n");
    let mut keys: Vec<&String> = dict.keys().collect();
    keys.sort();
    for key in keys {
        indent(out, depth + 1);
        out.push_str("<key>");
        escape(out, key);
        out.push_str("</key>\n");
        write_value(out, &dict[key.as_str()], depth + 1);
    }
    indent(out, depth);
    out.push_str("</dict>\n");
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push('\t');
    }
}

fn escape(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

fn write_value(out: &mut String, value: &Value, depth: usize) {
    indent(out, depth);
    match value {
        Value::Dictionary(dict) => write_dictionary(out, dict, depth),
        Value::Array(items) if items.is_empty() => out.push_str("<array/>\n"),
        Value::Array(items) => {
            out.push_str("<array>\n");
            for item in items {
                write_value(out, item, depth + 1);
            }
            indent(out, depth);
            out.push_str("</array>\n");
        }
        Value::String(text) => {
            out.push_str("<string>");
            escape(out, text);
            out.push_str("</string>\n");
        }
        Value::Boolean(true) => out.push_str("<true/>\n"),
        Value::Boolean(false) => out.push_str("<false/>\n"),
        Value::Integer(n) => {
            let _ = match (n.as_signed(), n.as_unsigned()) {
                (Some(v), _) => writeln!(out, "<integer>{v}</integer>"),
                (None, Some(v)) => writeln!(out, "<integer>{v}</integer>"),
                (None, None) => writeln!(out, "<integer>0</integer>"),
            };
        }
        Value::Real(r) => {
            let _ = writeln!(out, "<real>{}</real>", real(*r));
        }
        Value::Date(date) => {
            let time: std::time::SystemTime = (*date).into();
            let _ = writeln!(out, "<date>{}</date>", iso8601(time));
        }
        Value::Data(bytes) => {
            out.push_str("<data>\n");
            let text = crate::base64::encode(bytes, 0);
            for line in text.as_bytes().chunks(76) {
                indent(out, depth);
                out.push_str(std::str::from_utf8(line).unwrap_or(""));
                out.push('\n');
            }
            indent(out, depth);
            out.push_str("</data>\n");
        }
        Value::Uid(uid) => {
            let _ = writeln!(
                out,
                "<dict>\n{0}\t<key>CF$UID</key>\n{0}\t<integer>{1}</integer>\n{0}</dict>",
                "\t".repeat(depth),
                uid.get()
            );
        }
        _ => out.push_str("<string></string>\n"),
    }
}

/// A real as macOS writes it: `%.17g`, with 0 as `0.0` and the special
/// values spelled out.
pub(crate) fn real(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "+infinity".into() } else { "-infinity".into() };
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    format_g(value, 17)
}

/// C's `%.{precision}g`.
fn format_g(value: f64, precision: usize) -> String {
    let scientific = format!("{:.*e}", precision - 1, value);
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let trim = |s: &str| -> String {
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s.to_string() }
    };
    if exponent < -4 || exponent >= precision as i32 {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exponent.abs())
    } else {
        let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
        trim(&format!("{value:.decimals$}"))
    }
}

/// `2001-01-01T00:00:00Z`, whole seconds.
fn iso8601(time: std::time::SystemTime) -> String {
    let secs = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs_f64().ceil() as i64),
    };
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = crate::date::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

/// A Foundation object for a property-list value; `None` for what can't
/// be made yet (numbers and arrays without collections) or at all (UIDs).
pub(crate) fn to_object(value: &Value) -> Option<Retained<AnyObject>> {
    match value {
        Value::String(text) => Some(NSString::from_str(text).into()),
        Value::Dictionary(dict) => {
            let mut keys = Vec::with_capacity(dict.len());
            let mut values = Vec::with_capacity(dict.len());
            for (key, value) in dict.iter() {
                if let Some(object) = to_object(value) {
                    keys.push(NSString::from_str(key));
                    values.push(object);
                }
            }
            let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            Some(NSDictionary::from_retained_objects(&keys, &values).into())
        }
        Value::Data(bytes) => Some(NSData::from_vec(bytes.clone()).into()),
        Value::Date(date) => {
            let time: std::time::SystemTime = (*date).into();
            let unix = match time.duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => d.as_secs_f64(),
                Err(e) => -e.duration().as_secs_f64(),
            };
            Some(NSDate::dateWithTimeIntervalSinceReferenceDate(unix - crate::date::UNIX_TO_REFERENCE).into())
        }
        Value::Array(items) => {
            let items: Vec<Retained<AnyObject>> = items.iter().filter_map(to_object).collect();
            Some(objc2_foundation::NSArray::from_retained_slice(&items).into())
        }
        Value::Boolean(b) => Some(objc2_foundation::NSNumber::new_bool(*b).into()),
        Value::Integer(n) => Some(match (n.as_signed(), n.as_unsigned()) {
            (Some(v), _) => objc2_foundation::NSNumber::new_i64(v).into(),
            (None, Some(v)) => objc2_foundation::NSNumber::new_u64(v).into(),
            (None, None) => return None,
        }),
        Value::Real(r) => Some(objc2_foundation::NSNumber::new_f64(*r).into()),
        _ => None,
    }
}

/// The property-list value of a Foundation object; `None` for objects
/// that aren't property lists (or hold something that isn't).
pub(crate) fn from_object(object: &AnyObject) -> Option<Value> {
    if let Some(text) = object.downcast_ref::<NSString>() {
        return Some(Value::String(text.to_string()));
    }
    if let Some(data) = object.downcast_ref::<NSData>() {
        return Some(Value::Data(crate::data::to_vec(data)));
    }
    if let Some(date) = object.downcast_ref::<NSDate>() {
        let unix = crate::date::time_of(date) + crate::date::UNIX_TO_REFERENCE;
        let time = if unix >= 0.0 {
            std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(unix)
        } else {
            std::time::UNIX_EPOCH - std::time::Duration::from_secs_f64(-unix)
        };
        return Some(Value::Date(time.into()));
    }
    if let Some(dict) = object.downcast_ref::<NSDictionary>() {
        let mut out = plist::Dictionary::new();
        for (key, value) in dictionary_entries(dict) {
            let key = key.downcast_ref::<NSString>()?.to_string();
            out.insert(key, from_object(&value)?);
        }
        return Some(Value::Dictionary(out));
    }
    if let Some(array) = object.downcast_ref::<objc2_foundation::NSArray>() {
        let items: Option<Vec<Value>> = (0..array.count()).map(|i| from_object(&array.objectAtIndex(i))).collect();
        return items.map(Value::Array);
    }
    if let Some(number) = object.downcast_ref::<objc2_foundation::NSNumber>() {
        return Some(number_value(number));
    }
    None
}

pub(crate) fn number_value(number: &objc2_foundation::NSNumber) -> Value {
    // SAFETY: -objCType returns a C string that lives as long as the number.
    let kind = unsafe { std::ffi::CStr::from_ptr(number.objCType().as_ptr()) }.to_bytes().first().copied();
    match kind {
        Some(b'f' | b'd') => Value::Real(number.doubleValue()),
        Some(b'c' | b'B') if matches!(number.longLongValue(), 0 | 1) => Value::Boolean(number.boolValue()),
        Some(b'Q' | b'L' | b'I' | b'S' | b'C') => Value::Integer(number.unsignedLongLongValue().into()),
        _ => Value::Integer(number.longLongValue().into()),
    }
}

/// A dictionary's entries.
pub(crate) fn dictionary_entries(dict: &NSDictionary) -> Vec<(Retained<AnyObject>, Retained<AnyObject>)> {
    // SAFETY: -count takes nothing.
    let count: NSUInteger = unsafe { msg_send![dict, count] };
    let mut keys: Vec<*mut AnyObject> = vec![std::ptr::null_mut(); count];
    let mut values: Vec<*mut AnyObject> = vec![std::ptr::null_mut(); count];
    // SAFETY: both buffers hold `count` objects.
    let () = unsafe { msg_send![dict, getObjects: values.as_mut_ptr(), andKeys: keys.as_mut_ptr(), count: count] };
    keys.into_iter()
        .zip(values)
        .filter_map(|(k, v)| {
            // SAFETY: the dictionary's own objects, retained here.
            unsafe { Some((Retained::retain(k)?, Retained::retain(v)?)) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reals() {
        for (value, text) in [
            (2.0, "2"),
            (1.5, "1.5"),
            (0.1, "0.10000000000000001"),
            (1e20, "1e+20"),
            (0.0, "0.0"),
            (f64::NAN, "nan"),
            (f64::INFINITY, "+infinity"),
            (-2.5e-7, "-2.4999999999999999e-07"),
            (123456.0, "123456"),
        ] {
            assert_eq!(real(value), text, "{value}");
        }
    }

    #[test]
    fn xml_layout() {
        let mut dict = plist::Dictionary::new();
        dict.insert("zeta".into(), Value::String("last".into()));
        dict.insert("alpha".into(), Value::Integer(42.into()));
        dict.insert("arr".into(), Value::Array(vec![Value::String("x".into()), Value::String("y".into())]));
        dict.insert("data".into(), Value::Data(b"ab".to_vec()));
        dict.insert("empty".into(), Value::Dictionary(plist::Dictionary::new()));
        dict.insert("esc".into(), Value::String("<&>\"'".into()));
        dict.insert("mid".into(), Value::Boolean(true));
        dict.insert(
            "date".into(),
            Value::Date((std::time::UNIX_EPOCH + std::time::Duration::from_secs(978_307_200)).into()),
        );
        assert_eq!(write_xml_dictionary(&dict), write_xml(&Value::Dictionary(dict.clone())));
        let empty = plist::Dictionary::new();
        assert_eq!(write_xml_dictionary(&empty), write_xml(&Value::Dictionary(empty.clone())));
        let text = String::from_utf8(write_xml(&Value::Dictionary(dict))).unwrap();
        let expected = format!(
            "{XML_HEADER}<dict>\n\t<key>alpha</key>\n\t<integer>42</integer>\n\t<key>arr</key>\n\t<array>\n\t\t<string>x</string>\n\t\t<string>y</string>\n\t</array>\n\t<key>data</key>\n\t<data>\n\tYWI=\n\t</data>\n\t<key>date</key>\n\t<date>2001-01-01T00:00:00Z</date>\n\t<key>empty</key>\n\t<dict/>\n\t<key>esc</key>\n\t<string>&lt;&amp;&gt;\"'</string>\n\t<key>mid</key>\n\t<true/>\n\t<key>zeta</key>\n\t<string>last</string>\n</dict>\n</plist>\n"
        );
        assert_eq!(text, expected);
        let (back, format) = parse(text.as_bytes()).unwrap();
        assert_eq!(format, XML);
        assert_eq!(write_xml(&back), text.as_bytes());
        let binary = write_binary(&back).unwrap();
        assert_eq!(parse(&binary).unwrap(), (back, BINARY));
        let (ascii, format) = parse(b"{ a = b; c = (1, 2); }").unwrap();
        assert_eq!(format, OPENSTEP);
        assert!(matches!(ascii, Value::Dictionary(_)));
        assert!(parse(b"not a plist <").is_none());
    }
}
