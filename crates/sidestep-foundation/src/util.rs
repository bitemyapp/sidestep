//! What the collections share: exact-class checks, which let them read
//! Sidestep's own objects without sending messages, object equality with
//! those fast paths, and the errors Foundation raises.

use std::ptr;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{Message, msg_send};
use objc2_foundation::NSString;
use sidestep_runtime::Class;

use crate::number;
use crate::string::fast_parts;

/// Whether `obj`'s class is exactly `class`, not a subclass: the test for
/// "this object's storage is laid out as Sidestep's".
#[inline]
pub(crate) fn is_exactly(obj: &AnyObject, class: &Class) -> bool {
    // SAFETY: every object starts with its class pointer.
    let isa = unsafe { *(obj as *const AnyObject).cast::<*const Class>() };
    ptr::eq(isa, class)
}

/// Whether `obj`'s class is `class` or inherits from it, found without
/// messages: the test for "this object has `class`'s storage".
pub(crate) fn inherits(obj: &AnyObject, class: &Class) -> bool {
    let mut next = Some(obj.class());
    while let Some(c) = next {
        if ptr::eq((c as *const AnyClass).cast::<Class>(), class) {
            return true;
        }
        next = c.superclass();
    }
    false
}

/// `[obj isKindOfClass:class]`.
pub(crate) fn is_kind(obj: &AnyObject, class: &AnyClass) -> bool {
    // SAFETY: -isKindOfClass: takes a class and returns BOOL.
    unsafe { msg_send![obj, isKindOfClass: class] }
}

/// `[a isEqual:b]`. Identical objects are equal without a message, as in
/// CoreFoundation, and so are Sidestep's own strings and numbers. Strings
/// keep their hashes, so unequal ones mostly differ there.
#[inline]
pub(crate) fn equal(a: &AnyObject, b: &AnyObject) -> bool {
    if ptr::eq(a, b) {
        return true;
    }
    if let Some((x, hx)) = fast_parts(a) {
        if let Some((y, hy)) = fast_parts(b) {
            return hx == hy && x == y;
        }
    } else if let (Some(x), Some(y)) = (number::fast_value(a), number::fast_value(b)) {
        return x.equals(&y);
    }
    // SAFETY: -isEqual: takes an object and returns BOOL.
    unsafe { msg_send![a, isEqual: b] }
}

/// An object to compare many others with, as `[object isEqual:other]`,
/// with what lets it skip the message worked out once.
pub(crate) struct Needle<'a> {
    object: &'a AnyObject,
    fast: Fast<'a>,
}

enum Fast<'a> {
    Text(&'a str, usize),
    Number(number::Number),
    Other,
}

impl<'a> Needle<'a> {
    pub(crate) fn new(object: &'a AnyObject) -> Self {
        let fast = if let Some((text, hash)) = fast_parts(object) {
            Fast::Text(text, hash)
        } else if let Some(n) = number::fast_value(object) {
            Fast::Number(n)
        } else {
            Fast::Other
        };
        Needle { object, fast }
    }

    #[inline]
    pub(crate) fn matches(&self, other: &AnyObject) -> bool {
        if ptr::eq(self.object, other) {
            return true;
        }
        match self.fast {
            Fast::Text(text, hash) => {
                if let Some((t, h)) = fast_parts(other) {
                    return h == hash && t == text;
                }
            }
            Fast::Number(n) => {
                if let Some(m) = number::fast_value(other) {
                    return n.equals(&m);
                }
            }
            Fast::Other => {}
        }
        // SAFETY: -isEqual: takes an object and returns BOOL.
        unsafe { msg_send![self.object, isEqual: other] }
    }
}

/// A copy of a key, as dictionaries take them. Sidestep's strings and
/// numbers are immutable, so a copy of one is itself.
pub(crate) fn copy_key(key: &AnyObject) -> Retained<AnyObject> {
    if fast_parts(key).is_some() || number::fast_value(key).is_some() {
        key.retain()
    } else {
        // SAFETY: keys conform to NSCopying; -copy returns +1.
        unsafe { msg_send![key, copy] }
    }
}

/// Any object as an `AnyObject`.
pub(crate) fn upcast<T: Message>(obj: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every object is an AnyObject.
    unsafe { Retained::cast_unchecked(obj) }
}

/// An object's `-description` as Rust text.
pub(crate) fn description(obj: &AnyObject) -> String {
    if let Some((text, _)) = fast_parts(obj) {
        return text.to_owned();
    }
    if let Some(n) = number::fast_value(obj) {
        return n.to_string();
    }
    // SAFETY: -description returns an NSString, or nil before Foundation's
    // strings exist (which they do by now).
    let text: Option<Retained<NSString>> = unsafe { msg_send![obj, description] };
    text.map(|t| t.to_string()).unwrap_or_default()
}

/// Fail as Foundation's `NSRangeException` for an index does, with its
/// message: `*** -[NSArray objectAtIndex:]: index 5 beyond bounds [0 .. 1]`.
#[cold]
#[track_caller]
pub(crate) fn index_out_of_bounds(receiver: &str, method: &str, index: usize, count: usize) -> ! {
    index_beyond(receiver, method, index, count, "array")
}

/// Fail as `index_out_of_bounds` does, naming what an empty receiver is
/// ("array", "ordered set").
#[cold]
#[track_caller]
pub(crate) fn index_beyond(receiver: &str, method: &str, index: usize, count: usize, noun: &str) -> ! {
    if count == 0 {
        panic!("*** -[{receiver} {method}]: index {index} beyond bounds for empty {noun}");
    }
    panic!("*** -[{receiver} {method}]: index {index} beyond bounds [0 .. {}]", count - 1);
}

/// Fail as Foundation's `NSRangeException` for an index set reaching past
/// an array's end does.
#[cold]
#[track_caller]
pub(crate) fn index_set_out_of_bounds(receiver: &str, method: &str, index: usize, count: usize) -> ! {
    index_set_beyond(receiver, method, index, count, "array")
}

/// `index_set_out_of_bounds`, naming what an empty receiver is.
#[cold]
#[track_caller]
pub(crate) fn index_set_beyond(receiver: &str, method: &str, index: usize, count: usize, noun: &str) -> ! {
    if count == 0 {
        panic!("*** -[{receiver} {method}]: index {index} in index set beyond bounds for empty {noun}");
    }
    panic!("*** -[{receiver} {method}]: index {index} in index set beyond bounds [0 .. {}]", count - 1);
}

/// Fail as Foundation's `NSRangeException` for a range does.
#[cold]
#[track_caller]
pub(crate) fn range_out_of_bounds(receiver: &str, method: &str, location: usize, length: usize, count: usize) -> ! {
    range_beyond(receiver, method, location, length, count, "array")
}

/// `range_out_of_bounds`, naming what an empty receiver is.
#[cold]
#[track_caller]
pub(crate) fn range_beyond(
    receiver: &str,
    method: &str,
    location: usize,
    length: usize,
    count: usize,
    noun: &str,
) -> ! {
    if count == 0 {
        panic!("*** -[{receiver} {method}]: range {{{location}, {length}}} extends beyond bounds for empty {noun}");
    }
    panic!("*** -[{receiver} {method}]: range {{{location}, {length}}} extends beyond bounds [0 .. {}]", count - 1);
}

/// Fail as Foundation's `NSInvalidArgumentException` for a nil argument
/// does: `*** -[NSMutableArray insertObject:atIndex:]: object cannot be nil`.
#[cold]
#[track_caller]
pub(crate) fn nil_argument(receiver: &str, method: &str, what: &str) -> ! {
    panic!("*** -[{receiver} {method}]: {what} cannot be nil");
}

/// A collection mutated while one of its own methods was reading it, from
/// inside a callback such as an element's `-isEqual:`.
#[cold]
#[track_caller]
pub(crate) fn mutated_while_reading(receiver: &str, obj: *const AnyObject) -> ! {
    panic!("*** Collection <{receiver}: {obj:p}> was mutated while being enumerated.");
}
