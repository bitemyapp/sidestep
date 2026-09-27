//! Property values as plain data, made into the dictionaries, arrays,
//! numbers and strings ImageIO hands out only when asked for: what's read
//! of a file stays `Send` data a source can keep, and each copy the
//! program asks for is a new dictionary, as ImageIO's Copy functions give.
//!
//! Numbers keep the types ImageIO gives them (measured on macOS): pixel
//! sizes and depths are `long long`s, orientations and most tags `int`s,
//! densities in dots per inch `float`s, delays and rationals `double`s,
//! and flags the boolean constants.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum P {
    /// A `long long`.
    Long(i64),
    /// An `int`.
    Int(i32),
    Float(f32),
    Double(f64),
    Bool(bool),
    Str(String),
    Array(Vec<P>),
    Dict(Dict),
}

/// A dictionary's entries, keys as ImageIO's key constants spell them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Dict(pub Vec<(&'static str, P)>);

impl Dict {
    pub fn new() -> Dict {
        Dict(Vec::new())
    }

    /// Set `key` (replacing what it had).
    pub fn set(&mut self, key: &'static str, value: P) {
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key, value)),
        }
    }

    pub fn get(&self, key: &str) -> Option<&P> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// An `NSDictionary` of the entries.
    pub fn to_object(&self) -> Retained<NSDictionary<NSString, AnyObject>> {
        let keys: Vec<Retained<NSString>> = self.0.iter().map(|(k, _)| NSString::from_str(k)).collect();
        let values: Vec<Retained<AnyObject>> = self.0.iter().map(|(_, v)| v.to_object()).collect();
        let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let values: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
        NSDictionary::from_slices(&keys, &values)
    }
}

fn any<T: objc2::Message>(object: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every object is an AnyObject.
    unsafe { Retained::cast_unchecked(object) }
}

impl P {
    pub fn to_object(&self) -> Retained<AnyObject> {
        match self {
            P::Long(n) => any(NSNumber::new_i64(*n)),
            P::Int(n) => any(NSNumber::new_i32(*n)),
            P::Float(n) => any(NSNumber::new_f32(*n)),
            P::Double(n) => any(NSNumber::new_f64(*n)),
            P::Bool(b) => any(NSNumber::new_bool(*b)),
            P::Str(s) => any(NSString::from_str(s)),
            P::Array(items) => {
                let items: Vec<Retained<AnyObject>> = items.iter().map(P::to_object).collect();
                any(NSArray::from_retained_slice(&items))
            }
            P::Dict(d) => any(d.to_object()),
        }
    }
}

/// Reading the dictionaries programs pass in (options and properties):
/// values by key, numbers and flags read the way CoreFoundation's
/// `CFNumberGetValue` and `CFBooleanGetValue` would.
pub(crate) mod read {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_core_foundation::{CFDictionary, CFString};
    use objc2_foundation::{NSDictionary, NSNumber, NSString};

    pub(crate) type Options = NSDictionary<NSString, AnyObject>;

    /// A CFDictionary as the NSDictionary it is here.
    pub(crate) fn dict(d: Option<&CFDictionary>) -> Option<&Options> {
        // SAFETY: a CFDictionary is an NSDictionary here.
        d.map(|d| unsafe { &*(d as *const CFDictionary).cast::<Options>() })
    }

    /// A CFString key as the NSString it is here.
    pub(crate) fn key(k: &CFString) -> &NSString {
        // SAFETY: a CFString is an NSString here.
        unsafe { &*(k as *const CFString).cast::<NSString>() }
    }

    pub(crate) fn value(d: Option<&Options>, k: &CFString) -> Option<Retained<AnyObject>> {
        d?.objectForKey(key(k))
    }

    /// A number (or anything answering `doubleValue`, as a string does).
    pub(crate) fn number(d: Option<&Options>, k: &CFString) -> Option<f64> {
        let v = value(d, k)?;
        let answers = v.class().responds_to(objc2::sel!(doubleValue));
        // SAFETY: the receiver answers -doubleValue, checked above.
        answers.then(|| unsafe { msg_send![&*v, doubleValue] })
    }

    /// A flag: a boolean, or a number (non-zero is true); `None` for
    /// neither.
    pub(crate) fn flag(d: Option<&Options>, k: &CFString) -> Option<bool> {
        let v = value(d, k)?;
        v.downcast_ref::<NSNumber>().map(|n| n.as_bool())
    }

    /// A nested dictionary.
    pub(crate) fn sub(d: Option<&Options>, k: &CFString) -> Option<Retained<Options>> {
        value(d, k)?.downcast::<NSDictionary>().ok().map(|d| {
            // SAFETY: the dictionaries ImageIO takes have string keys.
            unsafe { Retained::cast_unchecked::<Options>(d) }
        })
    }
}
