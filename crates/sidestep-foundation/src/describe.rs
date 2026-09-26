//! `-description` for collections, in the old property-list style
//! Foundation prints them in:
//!
//! ```text
//! (
//!     1,
//!     "two words",
//!         {
//!         key = value;
//!     }
//! )
//! ```
//!
//! Each level indents by four spaces. Arrays and dictionaries nested in a
//! collection describe themselves at the next level (through
//! `-descriptionWithLocale:indent:`); anything else contributes its
//! `-description`, quoted unless it is a plain run of ASCII letters and
//! digits. Dictionaries whose keys are all strings list them sorted as
//! Foundation's `-compare:` orders strings: by canonical decomposition, so
//! a precomposed letter sorts with its base letter, then by code point.

use std::fmt::Write;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::sel;
use objc2_foundation::NSString;
use unicode_normalization::UnicodeNormalization;

use crate::number::fast_value;
use crate::string::fast_parts;
use crate::util::{description, is_exactly};
use crate::{array, dictionary};

fn pad(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("    ");
    }
}

/// Append `obj` as an element or key at `level`.
pub(crate) fn element(out: &mut String, obj: &AnyObject, level: usize) {
    if let Some((text, _)) = fast_parts(obj) {
        quote(out, text);
    } else if let Some(n) = fast_value(obj) {
        quote(out, &n.to_string());
    } else if is_exactly(obj, &crate::NSARRAY) || is_exactly(obj, &crate::NSMUTABLEARRAY) {
        array::describe(out, obj, level);
    } else if is_exactly(obj, &crate::NSDICTIONARY) || is_exactly(obj, &crate::NSMUTABLEDICTIONARY) {
        dictionary::describe(out, obj, level);
    } else if responds(obj, sel!(descriptionWithLocale:indent:)) {
        // SAFETY: the selector returns an NSString.
        let text: Option<Retained<NSString>> =
            unsafe { msg_send![obj, descriptionWithLocale: None::<&AnyObject>, indent: level] };
        if let Some(text) = text {
            out.push_str(&text.to_string());
        }
    } else {
        quote(out, &description(obj));
    }
}

fn responds(obj: &AnyObject, sel: Sel) -> bool {
    // SAFETY: -respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![obj, respondsToSelector: sel] }
}

/// A list of elements between `open` and `close`, one per line.
pub(crate) fn list<'a>(
    out: &mut String,
    open: &str,
    close: &str,
    elements: impl IntoIterator<Item = &'a AnyObject>,
    level: usize,
) {
    pad(out, level);
    out.push_str(open);
    out.push('\n');
    let mut any = false;
    for obj in elements {
        if any {
            out.push_str(",\n");
        }
        any = true;
        pad(out, level + 1);
        element(out, obj, level + 1);
    }
    if any {
        out.push('\n');
    }
    pad(out, level);
    out.push_str(close);
}

/// `{ key = value; ... }`, sorted by key when every key is a string.
pub(crate) fn map<'a>(out: &mut String, pairs: impl IntoIterator<Item = (&'a AnyObject, &'a AnyObject)>, level: usize) {
    let mut pairs: Vec<(&AnyObject, &AnyObject)> = pairs.into_iter().collect();
    let texts: Option<Vec<String>> = pairs.iter().map(|(k, _)| string_text(k).map(|t| sort_key(&t))).collect();
    if let Some(texts) = texts {
        let mut order: Vec<usize> = (0..pairs.len()).collect();
        order.sort_by(|&a, &b| texts[a].cmp(&texts[b]));
        pairs = order.into_iter().map(|i| pairs[i]).collect();
    }
    pad(out, level);
    out.push_str("{\n");
    for (key, value) in pairs {
        pad(out, level + 1);
        element(out, key, level + 1);
        out.push_str(" = ");
        element(out, value, level + 1);
        out.push_str(";\n");
    }
    pad(out, level);
    out.push('}');
}

/// What a key sorts by: its canonical decomposition, which Rust's `str`
/// ordering then compares by code point. Plain ASCII needs no change.
fn sort_key(text: &str) -> String {
    if text.is_ascii() { text.to_owned() } else { text.nfd().collect() }
}

/// The text of any NSString, or `None` for other objects.
fn string_text(obj: &AnyObject) -> Option<String> {
    if let Some((text, _)) = fast_parts(obj) {
        return Some(text.to_owned());
    }
    // SAFETY: -isKindOfClass: takes a class and returns BOOL.
    let is_string: bool = unsafe { msg_send![obj, isKindOfClass: <NSString as objc2::ClassType>::class()] };
    // SAFETY: just checked that it is an NSString.
    is_string.then(|| unsafe { &*(obj as *const AnyObject).cast::<NSString>() }.to_string())
}

/// Append `text`, quoted and escaped unless it is letters and digits only.
pub(crate) fn quote(out: &mut String, text: &str) {
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphanumeric()) {
        out.push_str(text);
        return;
    }
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{b}' => out.push_str("\\v"),
            '\u{c}' => out.push_str("\\f"),
            // Foundation's C string conversion stops short of NULs; it
            // drops them.
            '\0' => {}
            c if c.is_ascii() => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\U{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::quote;

    fn quoted(s: &str) -> String {
        let mut out = String::new();
        quote(&mut out, s);
        out
    }

    #[test]
    fn quoting_matches_foundation() {
        assert_eq!(quoted("abc1"), "abc1");
        assert_eq!(quoted(""), "\"\"");
        assert_eq!(quoted("a_b"), "\"a_b\"");
        assert_eq!(quoted("héllo"), "\"h\\U00e9llo\"");
        assert_eq!(quoted("🎉"), "\"\\Ud83c\\Udf89\"");
        assert_eq!(quoted("a\"b\\c\nd\te\r"), "\"a\\\"b\\\\c\\nd\\te\r\"");
        assert_eq!(quoted("a\0b"), "\"ab\"");
    }
}
