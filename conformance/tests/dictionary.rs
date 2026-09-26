//! `NSDictionary` behavior, checked on macOS and on Linux alike.

use std::cell::Cell;
use std::collections::HashSet;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{CopyingHelper, NSCopying, NSDictionary, NSString, NSUInteger, NSZone};

use sidestep as _;

thread_local!(static COPIES: Cell<usize> = const { Cell::new(0) });

struct KeyIvars {
    id: usize,
    hash: NSUInteger,
}

define_class!(
    /// A key with its own hash and equality, counting its copies.
    #[unsafe(super(NSObject))]
    #[name = "ConformanceKey"]
    #[ivars = KeyIvars]
    struct Key;

    unsafe impl NSObjectProtocol for Key {
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            self.ivars().hash
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|o| {
                o.class() == Key::class() && {
                    // SAFETY: just checked the class.
                    let o = unsafe { &*(o as *const AnyObject).cast::<Key>() };
                    o.ivars().id == self.ivars().id
                }
            })
        }
    }

    unsafe impl NSCopying for Key {
        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            COPIES.with(|c| c.set(c.get() + 1));
            Key::new(self.ivars().id, self.ivars().hash)
        }
    }
);

impl Key {
    fn new(id: usize, hash: NSUInteger) -> Retained<Self> {
        let this = Self::alloc().set_ivars(KeyIvars { id, hash });
        unsafe { msg_send![super(this), init] }
    }
}

fn long_key(i: usize) -> Retained<NSString> {
    // Long enough that Apple doesn't make it a tagged pointer.
    NSString::from_str(&format!("conformance-dictionary-key-{i}"))
}

// SAFETY: a Key's copy is a Key.
unsafe impl CopyingHelper for Key {
    type Result = Self;
}

fn build<K>(keys: &[Retained<K>], values: &[Retained<NSObject>]) -> Retained<NSDictionary<K, NSObject>>
where
    K: objc2::Message + NSCopying + CopyingHelper<Result = K>,
{
    let keys: Vec<&K> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&NSObject> = values.iter().map(|v| &**v).collect();
    NSDictionary::from_slices(&keys, &values)
}

fn same(a: Option<Retained<NSObject>>, b: &NSObject) -> bool {
    a.is_some_and(|a| std::ptr::eq(&*a, b))
}

#[test]
fn string_keys() {
    for n in [0, 1, 2, 7, 8, 9, 100, 1000] {
        let keys: Vec<_> = (0..n).map(long_key).collect();
        let values: Vec<_> = (0..n).map(|_| NSObject::new()).collect();
        let dict = build(&keys, &values);
        assert_eq!(dict.count(), n);
        for i in 0..n {
            assert!(same(dict.objectForKey(&keys[i]), &values[i]), "same key object, {i} of {n}");
            assert!(same(dict.objectForKey(&long_key(i)), &values[i]), "equal key, {i} of {n}");
        }
        assert!(dict.objectForKey(&long_key(n)).is_none());
        assert!(dict.objectForKey(ns_short("x")).is_none());
    }
}

fn ns_short(s: &str) -> &'static NSString {
    Box::leak(Box::new(NSString::from_str(s)))
}

#[test]
fn short_and_non_ascii_string_keys() {
    let texts = ["", "a", "ab", "héllo", "🎉", "a much longer key than the others"];
    let keys: Vec<_> = texts.iter().map(|t| NSString::from_str(t)).collect();
    let values: Vec<_> = texts.iter().map(|_| NSObject::new()).collect();
    let dict = build(&keys, &values);
    for (i, t) in texts.iter().enumerate() {
        assert!(same(dict.objectForKey(&NSString::from_str(t)), &values[i]), "{t:?}");
    }
}

#[test]
fn custom_keys_use_hash_and_is_equal() {
    // Every key hashes alike, so only -isEqual: tells them apart.
    let keys: Vec<_> = (0..50).map(|i| Key::new(i, 7)).collect();
    let values: Vec<_> = (0..50).map(|_| NSObject::new()).collect();
    COPIES.with(|c| c.set(0));
    let dict = build(&keys, &values);
    assert_eq!(COPIES.with(Cell::get), 50, "keys are copied");
    for (i, value) in values.iter().enumerate() {
        assert!(same(dict.objectForKey(&*Key::new(i, 7)), value));
    }
    assert!(dict.objectForKey(&*Key::new(50, 7)).is_none());
}

#[test]
fn duplicate_keys_keep_one_entry() {
    let keys = [long_key(1), long_key(2), long_key(1)];
    let values: Vec<_> = (0..3).map(|_| NSObject::new()).collect();
    let dict = build(&keys, &values);
    assert_eq!(dict.count(), 2);
    assert!(same(dict.objectForKey(&keys[1]), &values[1]));
    let one = dict.objectForKey(&keys[0]).expect("present");
    assert!(std::ptr::eq(&*one, &*values[0]) || std::ptr::eq(&*one, &*values[2]));
}

#[test]
fn contents_and_copies() {
    let keys: Vec<_> = (0..20).map(long_key).collect();
    let values: Vec<_> = (0..20).map(|_| NSObject::new()).collect();
    let dict = build(&keys, &values);
    let (ks, vs) = dict.to_vecs();
    assert_eq!((ks.len(), vs.len()), (20, 20));
    let pairs: HashSet<(String, usize)> =
        ks.iter().zip(&vs).map(|(k, v)| (k.to_string(), Retained::as_ptr(v) as usize)).collect();
    let expected: HashSet<(String, usize)> =
        keys.iter().zip(&values).map(|(k, v)| (k.to_string(), Retained::as_ptr(v) as usize)).collect();
    assert_eq!(pairs, expected);

    let copy = dict.copy();
    assert_eq!(copy.count(), 20);
    assert!(same(copy.objectForKey(&keys[5]), &values[5]));
}
