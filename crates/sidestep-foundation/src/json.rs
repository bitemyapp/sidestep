//! `NSJSONSerialization`, with its own reader and writer.
//!
//! The reader takes UTF-8 (with or without a byte-order mark), UTF-16 and
//! UTF-32, and is as lenient as macOS's where macOS is lenient (a comma
//! after the last element or member is fine; of repeated keys the first
//! wins) and strict where it is strict (no leading zeros, no `1.` or
//! `.5`, no unescaped control characters, no lone surrogates, no numbers
//! that overflow a double). Numbers with a fraction or an exponent are
//! doubles; others are integers.
//!
//! The writer escapes `/` unless asked not to, writes doubles as `%.17g`,
//! and pretty-prints with two-space indents and `" : "` between key and
//! value, an empty container as an open line, as macOS does.
//!
//! Arrays, numbers and `null` need `NSArray`, `NSNumber` and `NSNull`:
//! until Foundation's collections are in, JSON holding them fails to read
//! (with an error saying so) and objects holding them fail to write.

use std::fmt::Write as _;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{ClassType, define_class};
use objc2_foundation::{NSData, NSDictionary, NSError, NSString, NSUInteger};

sidestep_runtime::static_class!(
    pub(crate) NSJSONSERIALIZATION,
    NSJSONSERIALIZATION_META = "NSJSONSerialization",
    || {
        let _ = NSJSONSerializationImpl::class();
        crate::perform::install();
    }
);

/// `NSJSONReadingOptions`.
const FRAGMENTS_ALLOWED: NSUInteger = 1 << 2;
/// `NSJSONWritingOptions`.
const PRETTY_PRINTED: NSUInteger = 1 << 0;
const SORTED_KEYS: NSUInteger = 1 << 1;
const WRITING_FRAGMENTS_ALLOWED: NSUInteger = 1 << 2;
const WITHOUT_ESCAPING_SLASHES: NSUInteger = 1 << 3;

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Double(f64),
    String(String),
    Array(Vec<Json>),
    /// Members in document order, each key once.
    Object(Vec<(String, Json)>),
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSJSONSerialization"]
    pub(crate) struct NSJSONSerializationImpl;

    impl NSJSONSerializationImpl {
        #[unsafe(method_id(JSONObjectWithData:options:error:))]
        fn json_object_with_data(data: &NSData, options: NSUInteger, error: *mut *mut NSError) -> Option<Retained<AnyObject>> {
            // SAFETY: nothing mutates the data while it is parsed.
            let result = parse(unsafe { crate::data::bytes(data) }, options & FRAGMENTS_ALLOWED != 0)
                .and_then(|json| to_object(&json));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error) }
        }

        #[unsafe(method_id(dataWithJSONObject:options:error:))]
        fn data_with_json_object(object: &AnyObject, options: NSUInteger, error: *mut *mut NSError) -> Option<Retained<NSData>> {
            let result = from_object(object)
                .and_then(|json| {
                    let container = matches!(json, Json::Array(_) | Json::Object(_));
                    if container || options & WRITING_FRAGMENTS_ALLOWED != 0 {
                        Ok(json)
                    } else {
                        Err("Invalid top-level type in JSON write".to_string())
                    }
                })
                .map(|json| NSData::from_vec(write(&json, options).into_bytes()));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error) }
        }

        #[unsafe(method(isValidJSONObject:))]
        fn is_valid_json_object(object: &AnyObject) -> bool {
            matches!(from_object(object), Ok(Json::Array(_) | Json::Object(_)))
        }
    }
);

/// # Safety
///
/// `error` is null or writable.
unsafe fn report<T>(result: Result<T, String>, error: *mut *mut NSError) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(message) => {
            let info = [(&crate::error::DEBUG_DESCRIPTION, NSString::from_str(&message).into())];
            // SAFETY: per this function's contract.
            unsafe {
                crate::error::set(error, crate::error::cocoa(crate::error::code::PROPERTY_LIST_READ_CORRUPT, &info))
            };
            None
        }
    }
}

/// Text from JSON bytes in any of the encodings JSON allows.
fn decode(bytes: &[u8]) -> Result<String, String> {
    let utf16 = |bytes: &[u8], big: bool| -> Result<String, String> {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| if big { u16::from_be_bytes(c) } else { u16::from_le_bytes(c) })
            .collect();
        String::from_utf16(&units).map_err(|_| "Invalid UTF-16".to_string())
    };
    let utf32 = |bytes: &[u8], big: bool| -> Result<String, String> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&c| {
                let n = if big { u32::from_be_bytes(c) } else { u32::from_le_bytes(c) };
                char::from_u32(n).ok_or_else(|| "Invalid UTF-32".to_string())
            })
            .collect()
    };
    match bytes {
        [0xef, 0xbb, 0xbf, rest @ ..] => {
            std::str::from_utf8(rest).map(str::to_string).map_err(|_| "Invalid UTF-8".into())
        }
        [0x00, 0x00, 0xfe, 0xff, rest @ ..] => utf32(rest, true),
        [0xff, 0xfe, 0x00, 0x00, rest @ ..] => utf32(rest, false),
        [0xfe, 0xff, rest @ ..] => utf16(rest, true),
        [0xff, 0xfe, rest @ ..] => utf16(rest, false),
        // Without a mark, the zero bytes around the first ASCII character
        // tell the encoding.
        [0, 0, 0, _, ..] => utf32(bytes, true),
        [_, 0, 0, 0, ..] => utf32(bytes, false),
        [0, _, ..] => utf16(bytes, true),
        [_, 0, ..] => utf16(bytes, false),
        _ => std::str::from_utf8(bytes).map(str::to_string).map_err(|_| "Invalid UTF-8".into()),
    }
}

/// Parse JSON bytes.
pub(crate) fn parse(bytes: &[u8], fragments: bool) -> Result<Json, String> {
    if bytes.is_empty() {
        return Err("Unable to parse empty data.".into());
    }
    let text = decode(bytes)?;
    let mut parser = Parser { chars: text.chars().collect(), at: 0 };
    parser.skip_space();
    if !fragments && !matches!(parser.peek(), Some('[' | '{')) {
        return Err(parser.error("JSON text did not start with array or object and option to allow fragments not set."));
    }
    let value = parser.value()?;
    parser.skip_space();
    if parser.at < parser.chars.len() {
        return Err(parser.error("Garbage at end"));
    }
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn error(&self, what: &str) -> String {
        let before = &self.chars[..self.at.min(self.chars.len())];
        let line = before.iter().filter(|&&c| c == '\n').count() + 1;
        let column = before.iter().rev().take_while(|&&c| c != '\n').count();
        format!("{what} around line {line}, column {column}.")
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.at += 1;
        }
    }

    fn expect_word(&mut self, word: &str, value: Json) -> Result<Json, String> {
        for expected in word.chars() {
            match self.peek() {
                Some(c) if c == expected => self.at += 1,
                None => return Err(self.error("Unexpected end of file")),
                Some(_) => return Err(self.error("Invalid value")),
            }
        }
        Ok(value)
    }

    fn value(&mut self) -> Result<Json, String> {
        self.skip_space();
        match self.peek() {
            None => Err(self.error("Unexpected end of file")),
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') => self.string().map(Json::String),
            Some('t') => self.expect_word("true", Json::Bool(true)),
            Some('f') => self.expect_word("false", Json::Bool(false)),
            Some('n') => self.expect_word("null", Json::Null),
            Some('-' | '0'..='9') => self.number(),
            Some(_) => Err(self.error("Invalid value")),
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        self.at += 1;
        let mut items = Vec::new();
        loop {
            self.skip_space();
            match self.peek() {
                None => return Err(self.error("Unexpected end of file")),
                Some(']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                _ => {}
            }
            items.push(self.value()?);
            self.skip_space();
            match self.peek() {
                Some(',') => self.at += 1,
                Some(']') => {}
                None => return Err(self.error("Unexpected end of file")),
                Some(_) => return Err(self.error("Badly formed array")),
            }
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.at += 1;
        let mut members: Vec<(String, Json)> = Vec::new();
        loop {
            self.skip_space();
            match self.peek() {
                None => return Err(self.error("Unexpected end of file")),
                Some('}') => {
                    self.at += 1;
                    return Ok(Json::Object(members));
                }
                Some('"') => {}
                Some(_) => return Err(self.error("No string key for value in object")),
            }
            let key = self.string()?;
            self.skip_space();
            if self.peek() != Some(':') {
                return Err(self.error("No value for key in object"));
            }
            self.at += 1;
            let value = self.value()?;
            if !members.iter().any(|(k, _)| *k == key) {
                members.push((key, value));
            }
            self.skip_space();
            match self.peek() {
                Some(',') => self.at += 1,
                Some('}') => {}
                None => return Err(self.error("Unexpected end of file")),
                Some(_) => return Err(self.error("Badly formed object")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut n = 0;
        for _ in 0..4 {
            let digit =
                self.peek().and_then(|c| c.to_digit(16)).ok_or_else(|| self.error("Invalid escape sequence"))?;
            n = n * 16 + digit;
            self.at += 1;
        }
        Ok(n)
    }

    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else { return Err(self.error("Unexpected end of file during string parse")) };
            self.at += 1;
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let Some(escaped) = self.peek() else {
                        return Err(self.error("Unexpected end of file during string parse"));
                    };
                    self.at += 1;
                    match escaped {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let unit = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&unit) {
                                if self.peek() != Some('\\') || self.chars.get(self.at + 1) != Some(&'u') {
                                    return Err(self.error("Unexpected end of file during string parse (expected low-surrogate code point but did not find one)."));
                                }
                                self.at += 2;
                                let low = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&low) {
                                    return Err(self.error("Invalid low-surrogate code point"));
                                }
                                0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00)
                            } else {
                                unit
                            };
                            out.push(char::from_u32(code).ok_or_else(|| self.error("Invalid Unicode escape"))?);
                        }
                        _ => return Err(self.error("Invalid escape sequence")),
                    }
                }
                c if (c as u32) < 0x20 => return Err(self.error("Unescaped control character")),
                c => out.push(c),
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        if self.peek() == Some('-') {
            self.at += 1;
        }
        let int_start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        let int_len = self.at - int_start;
        if int_len == 0 {
            return Err(self.error("Invalid value"));
        }
        if int_len > 1 && self.chars[int_start] == '0' {
            self.at = int_start + 1;
            return Err(self.error("Number with leading zero"));
        }
        let mut floating = false;
        if self.peek() == Some('.') {
            self.at += 1;
            let frac = self.at;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.at += 1;
            }
            if self.at == frac {
                return Err(self.error("Number with decimal point but no additional digits"));
            }
            floating = true;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.at += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.at += 1;
            }
            let exp = self.at;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.at += 1;
            }
            if self.at == exp {
                return Err(self.error("Exponent with no digits"));
            }
            floating = true;
        }
        let text: String = self.chars[start..self.at].iter().collect();
        if !floating {
            if let Ok(n) = text.parse::<i64>() {
                return Ok(Json::Int(n));
            }
            if let Ok(n) = text.parse::<u64>() {
                return Ok(Json::UInt(n));
            }
        }
        let value: f64 = text.parse().map_err(|_| self.error("Invalid number"))?;
        if !value.is_finite() {
            let at = self.at;
            self.at = start;
            let message = self.error("Number wound up as NaN");
            self.at = at;
            return Err(message);
        }
        Ok(Json::Double(value))
    }
}

/// JSON text for a value.
pub(crate) fn write(value: &Json, options: NSUInteger) -> String {
    let mut out = String::new();
    write_value(&mut out, value, options, 0);
    out
}

fn write_string(out: &mut String, text: &str, options: NSUInteger) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' if options & WITHOUT_ESCAPING_SLASHES == 0 => out.push_str("\\/"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn newline(out: &mut String, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_value(out: &mut String, value: &Json, options: NSUInteger, depth: usize) {
    let pretty = options & PRETTY_PRINTED != 0;
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Json::UInt(n) => {
            let _ = write!(out, "{n}");
        }
        Json::Double(d) => out.push_str(&double(*d)),
        Json::String(s) => write_string(out, s, options),
        Json::Array(items) => {
            out.push('[');
            if pretty && items.is_empty() {
                out.push('\n');
                newline(out, depth);
            }
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if pretty {
                    newline(out, depth + 1);
                }
                write_value(out, item, options, depth + 1);
            }
            if pretty && !items.is_empty() {
                newline(out, depth);
            }
            out.push(']');
        }
        Json::Object(members) => {
            out.push('{');
            let mut members: Vec<&(String, Json)> = members.iter().collect();
            if options & SORTED_KEYS != 0 {
                members.sort_by(|a, b| a.0.cmp(&b.0));
            }
            if pretty && members.is_empty() {
                out.push('\n');
                newline(out, depth);
            }
            for (i, (key, value)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if pretty {
                    newline(out, depth + 1);
                }
                write_string(out, key, options);
                out.push_str(if pretty { " : " } else { ":" });
                write_value(out, value, options, depth + 1);
            }
            if pretty && !members.is_empty() {
                newline(out, depth);
            }
            out.push('}');
        }
    }
}

/// A double as macOS writes it in JSON: `%.17g`.
fn double(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    crate::plist::real(value)
}

/// A Foundation object for a JSON value. (`MutableContainers` waits for
/// Foundation's mutable collections; until then containers are
/// immutable.)
fn to_object(json: &Json) -> Result<Retained<AnyObject>, String> {
    match json {
        Json::String(s) => Ok(NSString::from_str(s).into()),
        Json::Object(members) => {
            let keys: Vec<Retained<NSString>> = members.iter().map(|(k, _)| NSString::from_str(k)).collect();
            let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let values = members.iter().map(|(_, v)| to_object(v)).collect::<Result<Vec<_>, _>>()?;
            Ok(NSDictionary::from_retained_objects(&keys, &values).into())
        }
        #[cfg(feature = "collections")]
        Json::Array(items) => {
            let items = items.iter().map(to_object).collect::<Result<Vec<_>, _>>()?;
            Ok(objc2_foundation::NSArray::from_retained_slice(&items).into())
        }
        #[cfg(feature = "collections")]
        Json::Null => Ok(objc2_foundation::NSNull::null().into()),
        #[cfg(feature = "collections")]
        Json::Bool(b) => Ok(objc2_foundation::NSNumber::new_bool(*b).into()),
        #[cfg(feature = "collections")]
        Json::Int(n) => Ok(objc2_foundation::NSNumber::new_i64(*n).into()),
        #[cfg(feature = "collections")]
        Json::UInt(n) => Ok(objc2_foundation::NSNumber::new_u64(*n).into()),
        #[cfg(feature = "collections")]
        Json::Double(d) => Ok(objc2_foundation::NSNumber::new_f64(*d).into()),
        #[cfg(not(feature = "collections"))]
        _ => Err("JSON arrays, numbers and null need Foundation's collections".into()),
    }
}

/// The JSON value of a Foundation object.
fn from_object(object: &AnyObject) -> Result<Json, String> {
    if let Some(text) = object.downcast_ref::<NSString>() {
        return Ok(Json::String(text.to_string()));
    }
    if let Some(dict) = object.downcast_ref::<NSDictionary>() {
        let mut members = Vec::new();
        for (key, value) in crate::plist::dictionary_entries(dict) {
            let key = key.downcast_ref::<NSString>().ok_or("Invalid (non-string) key in JSON dictionary")?.to_string();
            members.push((key, from_object(&value)?));
        }
        return Ok(Json::Object(members));
    }
    #[cfg(feature = "collections")]
    {
        if let Some(array) = object.downcast_ref::<objc2_foundation::NSArray>() {
            return (0..array.count())
                .map(|i| from_object(&array.objectAtIndex(i)))
                .collect::<Result<_, _>>()
                .map(Json::Array);
        }
        if object.downcast_ref::<objc2_foundation::NSNull>().is_some() {
            return Ok(Json::Null);
        }
        if let Some(number) = object.downcast_ref::<objc2_foundation::NSNumber>() {
            return match crate::plist::number_value(number) {
                plist::Value::Boolean(b) => Ok(Json::Bool(b)),
                plist::Value::Real(d) if !d.is_finite() => {
                    Err("Invalid number value (infinite or NaN) in JSON write".into())
                }
                plist::Value::Real(d) => Ok(Json::Double(d)),
                plist::Value::Integer(n) => {
                    Ok(n.as_signed().map_or_else(|| Json::UInt(n.as_unsigned().unwrap_or(0)), Json::Int))
                }
                _ => Err("Invalid number in JSON write".into()),
            };
        }
    }
    let class = object.class().name().to_string_lossy().into_owned();
    Err(format!("Invalid type in JSON write ({class})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(members: &[(&str, Json)]) -> Json {
        Json::Object(members.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    #[test]
    fn reading() {
        let parsed = parse(b"{\"a\":[1,2.5,-3,1e2,true,null,\"s\\u00e9\\n\"]}", false).unwrap();
        assert_eq!(
            parsed,
            object(&[(
                "a",
                Json::Array(vec![
                    Json::Int(1),
                    Json::Double(2.5),
                    Json::Int(-3),
                    Json::Double(100.0),
                    Json::Bool(true),
                    Json::Null,
                    Json::String("s\u{e9}\n".into())
                ])
            )])
        );
        assert_eq!(parse(b"[1,]", false).unwrap(), Json::Array(vec![Json::Int(1)]), "a trailing comma is fine");
        assert_eq!(parse(br#"{"a":1,}"#, false).unwrap(), object(&[("a", Json::Int(1))]));
        assert_eq!(parse(br#"{"a":1,"a":2}"#, false).unwrap(), object(&[("a", Json::Int(1))]), "the first wins");
        assert_eq!(parse("\u{feff}[1]".as_bytes(), false).unwrap(), Json::Array(vec![Json::Int(1)]));
        assert_eq!(
            parse(b"[12345678901234567890]", false).unwrap(),
            Json::Array(vec![Json::UInt(12345678901234567890)])
        );
        assert_eq!(parse(b"[\"\\ud83d\\ude00\"]", false).unwrap(), Json::Array(vec![Json::String("\u{1f600}".into())]));
        assert_eq!(parse(&[0xff, 0xfe, b'[', 0, b'1', 0, b']', 0], false).unwrap(), Json::Array(vec![Json::Int(1)]));
        assert_eq!(parse(b"[0, 0]", false).unwrap(), Json::Array(vec![Json::Int(0), Json::Int(0)]));
        assert_eq!(parse(b" 3 ", true).unwrap(), Json::Int(3));
        for bad in [
            &b"\"x\""[..],
            b"3",
            b"[01]",
            b"[1.]",
            b"[.5]",
            br#"["\ud83d"]"#,
            b"[1e400]",
            b"[NaN]",
            b"[\"a\x00b\"]",
            b"[\"tab\tin\"]",
            b"",
            b"[1] x",
            b"[true",
            b"{1:2}",
        ] {
            assert!(parse(bad, false).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        assert!(parse(b"[01]", false).unwrap_err().contains("line 1, column 2"));
    }

    #[test]
    fn writing() {
        let value = object(&[
            ("b", Json::Int(1)),
            ("a", Json::Array(vec![Json::String("x/y".into()), Json::String("\u{e9}\"\\\n\t\u{1}".into())])),
            ("c", Json::Null),
            ("e", Json::Object(vec![])),
            ("g", Json::Double(0.1)),
            ("h", Json::Double(1e20)),
            ("i", Json::Double(2.0)),
            ("k", Json::UInt(u64::MAX)),
            ("l", Json::Array(vec![])),
            ("m", Json::Double(1e-7)),
            ("t", Json::Bool(true)),
        ]);
        assert_eq!(
            write(&value, SORTED_KEYS),
            "{\"a\":[\"x\\/y\",\"\u{e9}\\\"\\\\\\n\\t\\u0001\"],\"b\":1,\"c\":null,\"e\":{},\"g\":0.10000000000000001,\"h\":1e+20,\"i\":2,\"k\":18446744073709551615,\"l\":[],\"m\":9.9999999999999995e-08,\"t\":true}"
        );
        assert_eq!(
            write(&value, SORTED_KEYS | PRETTY_PRINTED),
            "{\n  \"a\" : [\n    \"x\\/y\",\n    \"\u{e9}\\\"\\\\\\n\\t\\u0001\"\n  ],\n  \"b\" : 1,\n  \"c\" : null,\n  \"e\" : {\n\n  },\n  \"g\" : 0.10000000000000001,\n  \"h\" : 1e+20,\n  \"i\" : 2,\n  \"k\" : 18446744073709551615,\n  \"l\" : [\n\n  ],\n  \"m\" : 9.9999999999999995e-08,\n  \"t\" : true\n}"
        );
        assert!(write(&value, SORTED_KEYS | WITHOUT_ESCAPING_SLASHES).contains("\"x/y\""));
        assert_eq!(write(&Json::Object(vec![]), PRETTY_PRINTED), "{\n\n}");
        assert_eq!(write(&Json::Array(vec![Json::Array(vec![Json::Int(1)])]), PRETTY_PRINTED), "[\n  [\n    1\n  ]\n]");
        assert_eq!(
            write(&object(&[("z", Json::Int(1)), ("a", Json::Int(2))]), 0),
            "{\"z\":1,\"a\":2}",
            "unsorted keeps order"
        );
    }
}
