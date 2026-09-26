//! Attribute dictionaries: building new ones from old ones, and comparing
//! them by value.
//!
//! Dictionaries are reached through objc2's `NSDictionary`, so they work
//! with any dictionary class, and new ones are built with
//! `NSDictionary::from_slices`, which copies the keys.

use std::sync::atomic::{AtomicPtr, Ordering};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{Message, msg_send};
use objc2_foundation::{NSDictionary, NSString};

use super::Dict;

/// The empty attribute dictionary. There is one, shared by every run
/// without attributes, so such runs merge (as they do on macOS, where an
/// attribute removed from part of a run with no others left leaves text
/// that joins its unattributed neighbours).
pub(crate) fn empty() -> Retained<Dict> {
    static EMPTY: AtomicPtr<Dict> = AtomicPtr::new(std::ptr::null_mut());
    let mut p = EMPTY.load(Ordering::Acquire);
    if p.is_null() {
        // Kept for the life of the process; a lost race frees its copy.
        let new = Retained::into_raw(NSDictionary::new());
        p = match EMPTY.compare_exchange(std::ptr::null_mut(), new, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => new,
            Err(existing) => {
                // SAFETY: `new` was never shared.
                drop(unsafe { Retained::from_raw(new) });
                existing
            }
        };
    }
    // SAFETY: the shared dictionary is never released, and an immutable
    // dictionary may be used from any thread.
    unsafe { Retained::retain(p) }.expect("non-null")
}

/// An immutable copy of a caller's dictionary, so later changes to a
/// mutable one don't reach the runs. A copy of an immutable dictionary is
/// usually the dictionary itself.
pub(crate) fn copied(d: Option<&Dict>) -> Retained<Dict> {
    match d {
        Some(d) if d.count() > 0 => {
            // SAFETY: -copy of a dictionary is a dictionary with the same
            // keys and values.
            unsafe { msg_send![d, copy] }
        }
        _ => empty(),
    }
}

fn build(keys: &[&NSString], values: &[&AnyObject]) -> Retained<Dict> {
    if keys.is_empty() {
        return empty();
    }
    NSDictionary::from_slices(keys, values)
}

/// `d`'s keys and values, not retained: attribute dictionaries don't
/// change, and the references live no longer than `d`.
fn entries(d: &Dict) -> (Vec<&NSString>, Vec<&AnyObject>) {
    // SAFETY: nothing changes `d` while the references are used.
    unsafe { d.to_vecs_unchecked() }
}

/// `d` with `key` set to `value`.
pub(crate) fn with<'a>(d: &'a Dict, key: &'a NSString, value: &'a AnyObject) -> Retained<Dict> {
    let (mut keys, mut values) = entries(d);
    match keys.iter().position(|k| k.isEqualToString(key)) {
        Some(i) => values[i] = value,
        None => {
            keys.push(key);
            values.push(value);
        }
    }
    build(&keys, &values)
}

/// `d` with every entry of `other` added, replacing entries with the same
/// key.
pub(crate) fn merged(d: &Dict, other: &Dict) -> Retained<Dict> {
    let (mut keys, mut values) = entries(d);
    let (other_keys, other_values) = entries(other);
    for (k, v) in other_keys.into_iter().zip(other_values) {
        match keys.iter().position(|old| old.isEqualToString(k)) {
            Some(i) => values[i] = v,
            None => {
                keys.push(k);
                values.push(v);
            }
        }
    }
    build(&keys, &values)
}

/// `d` without `key`: `d` itself when it has no such key, so removing an
/// absent attribute leaves the runs as they were.
pub(crate) fn without(d: &Dict, key: &NSString) -> Retained<Dict> {
    if d.objectForKey(key).is_none() {
        return d.retain();
    }
    let (mut keys, mut values) = entries(d);
    if let Some(i) = keys.iter().position(|k| k.isEqualToString(key)) {
        keys.remove(i);
        values.remove(i);
    }
    build(&keys, &values)
}

/// Whether two values are equal the way attribute values compare: the same
/// object, or `-isEqual:`. Two nils are equal.
pub(crate) fn values_equal(a: Option<&AnyObject>, b: Option<&AnyObject>) -> bool {
    match (a, b) {
        (None, None) => true,
        // SAFETY: every object answers -isEqual:.
        (Some(a), Some(b)) => std::ptr::eq(a, b) || unsafe { msg_send![a, isEqual: b] },
        _ => false,
    }
}

/// Whether two dictionaries hold equal values for the same keys, compared
/// without relying on the dictionary class's own `-isEqual:`.
pub(crate) fn equal(a: &Dict, b: &Dict) -> bool {
    if std::ptr::eq(a, b) {
        return true;
    }
    if a.count() != b.count() {
        return false;
    }
    let (keys, values) = a.to_vecs();
    keys.iter().zip(&values).all(|(k, v)| values_equal(Some(v), b.objectForKey(k).as_deref()))
}
