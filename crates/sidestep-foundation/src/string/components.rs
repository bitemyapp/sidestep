//! Splitting, trimming and percent-encoding strings.
//!
//! `componentsSeparatedByString:` is a literal split (an empty separator
//! splits nothing); `componentsSeparatedByCharactersInSet:` splits at every
//! member character. Trimming tests single UTF-16 units against the set, as
//! Foundation does (see `trimmed`).

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class};
use objc2_foundation::{NSArray, NSCharacterSet, NSString};

use super::paths::new_array;
use super::view::view;
use super::wtf8::Pos;
use super::{inline, wtf8};
use crate::charset;

fn piece(bytes: &[u8]) -> Retained<NSString> {
    inline::new(bytes, wtf8::utf16_len(bytes), wtf8::flags_of(bytes, true))
}

fn this(obj: &Helper) -> &AnyObject {
    obj
}

/// Percent-encode every byte of the UTF-8 text but the ASCII characters in
/// `allowed`, as `%XX`: as on macOS, other characters are encoded whatever
/// the set holds. `None` for text holding a lone surrogate, which has no
/// UTF-8.
fn percent_encode(obj: &AnyObject, allowed: &AnyObject) -> Option<Retained<NSString>> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let v = view(obj);
    let t = v.text();
    let text = t.as_str()?;
    let m = charset::membership(allowed);
    let mut out = Vec::with_capacity(text.len() + text.len() / 4);
    for &byte in text.as_bytes() {
        if byte.is_ascii() && m.contains(u32::from(byte)) {
            out.push(byte);
        } else {
            out.extend_from_slice(&[b'%', HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 15)]]);
        }
    }
    Some(inline::new(&out, out.len(), wtf8::ASCII))
}

/// Decode `%XX` escapes. `None` when an escape is malformed or the bytes
/// aren't UTF-8.
fn percent_decode(obj: &AnyObject) -> Option<Retained<NSString>> {
    let v = view(obj);
    let t = v.text();
    let b = t.bytes;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let (hi, lo) = (hex(*b.get(i + 1)?)?, hex(*b.get(i + 2)?)?);
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    let s = std::str::from_utf8(&out).ok()?;
    Some(inline::new(s.as_bytes(), s.encode_utf16().count(), wtf8::flags_of(s.as_bytes(), false)))
}

/// Where the text left by trimming WTF-8 `bytes` starts and ends. Each
/// UTF-16 unit is tested on its own, as Foundation does: a character
/// outside the Basic Multilingual Plane goes when both its halves are
/// members, and is split when only the outer one is.
fn trimmed(bytes: &[u8], member: impl Fn(u32) -> bool) -> (Pos, Pos) {
    let halves = |c: u32| {
        let (high, low) = wtf8::split_pair(c);
        (u32::from(high), u32::from(low))
    };
    let mut start = Pos::at(bytes.len());
    for (at, c) in wtf8::code_points(bytes) {
        if c < 0x1_0000 {
            if !member(c) {
                start = Pos::at(at);
                break;
            }
            continue;
        }
        let (high, low) = halves(c);
        if !member(high) {
            start = Pos::at(at);
            break;
        }
        if !member(low) {
            start = Pos { byte: at, low: true };
            break;
        }
    }
    let mut end = Pos::at(bytes.len());
    let mut at = bytes.len();
    while at > start.byte {
        let s = wtf8::prev_boundary(bytes, at);
        let c = wtf8::decode(bytes, s).0;
        if c < 0x1_0000 {
            if !member(c) {
                break;
            }
        } else {
            let (high, low) = halves(c);
            // (A character split at the start kept its low half because it
            // isn't a member, so it stops this too.)
            if !member(low) {
                break;
            }
            if !member(high) {
                end = Pos { byte: s, low: true };
                break;
            }
        }
        at = s;
        end = Pos::at(s);
    }
    (start, end)
}

define_class!(
    // NSString's splitting and trimming methods, copied onto NSString when
    // it loads.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepStringComponents"]
    pub(crate) struct Helper;

    impl Helper {
        #[unsafe(method_id(componentsSeparatedByString:))]
        fn components_by_string(&self, separator: &NSString) -> Retained<NSArray<NSString>> {
            let parts = {
                let (hv, sv) = (view(this(self)), view(separator));
                let (h, s) = (hv.text(), sv.text());
                if s.bytes.is_empty() {
                    vec![piece(h.bytes)]
                } else {
                    let mut parts = Vec::new();
                    let mut last = 0;
                    for at in memchr::memmem::find_iter(h.bytes, s.bytes) {
                        if at < last {
                            continue;
                        }
                        parts.push(piece(&h.bytes[last..at]));
                        last = at + s.bytes.len();
                    }
                    parts.push(piece(&h.bytes[last..]));
                    parts
                }
            };
            new_array(&parts)
        }

        #[unsafe(method_id(componentsSeparatedByCharactersInSet:))]
        fn components_by_set(&self, set: &NSCharacterSet) -> Retained<NSArray<NSString>> {
            let parts = {
                let v = view(this(self));
                let t = v.text();
                let m = charset::membership(set);
                let mut parts = Vec::new();
                let mut last = 0;
                for (at, c) in wtf8::code_points(t.bytes) {
                    if m.contains(c) {
                        parts.push(piece(&t.bytes[last..at]));
                        last = at + wtf8::width(t.bytes[at]);
                    }
                }
                parts.push(piece(&t.bytes[last..]));
                parts
            };
            new_array(&parts)
        }

        #[unsafe(method_id(stringByTrimmingCharactersInSet:))]
        fn trimming(&self, set: &NSCharacterSet) -> Retained<NSString> {
            let obj = this(self);
            let v = view(obj);
            let t = v.text();
            let m = charset::membership(set);
            let (start, end) = trimmed(t.bytes, |u| m.contains(u));
            if start == Pos::at(0) && end == Pos::at(t.bytes.len()) {
                super::search::keep(obj, &v)
            } else {
                piece(&wtf8::slice(t.bytes, start, end))
            }
        }

        #[unsafe(method_id(stringByAddingPercentEncodingWithAllowedCharacters:))]
        fn adding_percent_encoding(&self, allowed: &NSCharacterSet) -> Option<Retained<NSString>> {
            percent_encode(this(self), allowed)
        }

        #[unsafe(method_id(stringByRemovingPercentEncoding))]
        fn removing_percent_encoding(&self) -> Option<Retained<NSString>> {
            percent_decode(this(self))
        }
    }
);

/// Add the splitting and trimming methods to NSString.
pub(crate) fn install(target: &AnyClass) {
    super::install::copy_methods(Helper::class(), target, false);
}
