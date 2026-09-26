//! `NSDictionary` and `NSMutableDictionary` behavior, checked on macOS and
//! on Linux alike.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{
    CopyingHelper, NSArray, NSComparisonResult, NSCopying, NSDictionary, NSMutableCopying, NSMutableDictionary,
    NSNumber, NSString, NSUInteger, NSZone,
};

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

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn n(value: i64) -> Retained<NSNumber> {
    NSNumber::new_i64(value)
}

fn description(obj: &AnyObject) -> String {
    let d: Retained<NSString> = unsafe { msg_send![obj, description] };
    d.to_string()
}

/// The reason an operation fails with: Apple raises an NSException;
/// Sidestep panics with the same message.
fn failure(f: impl FnOnce()) -> String {
    #[cfg(target_vendor = "apple")]
    {
        match objc2::exception::catch(AssertUnwindSafe(f)) {
            Ok(()) => panic!("expected an exception"),
            Err(Some(e)) => {
                let reason: Retained<NSString> = unsafe { msg_send![&*e, reason] };
                reason.to_string()
            }
            Err(None) => panic!("nil exception"),
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let e = std::panic::catch_unwind(AssertUnwindSafe(f)).expect_err("expected a panic");
        match e.downcast::<String>() {
            Ok(s) => *s,
            Err(e) => e.downcast::<&str>().map(|s| s.to_string()).unwrap_or_default(),
        }
    }
}

fn contents(dict: &NSDictionary<NSString, NSNumber>) -> HashMap<String, i64> {
    let (keys, values) = dict.to_vecs();
    keys.iter().zip(&values).map(|(k, v)| (k.to_string(), v.as_i64())).collect()
}

#[test]
fn mutable_dictionaries() {
    let m = NSMutableDictionary::<NSString, NSNumber>::new();
    assert_eq!(m.count(), 0);
    m.insert(&*s("a"), &n(1));
    m.insert(&*s("b"), &n(2));
    m.insert(&*s("a"), &n(3));
    assert_eq!(m.count(), 2);
    assert_eq!(m.objectForKey(&s("a")).unwrap().as_i64(), 3);
    m.removeObjectForKey(&s("a"));
    m.removeObjectForKey(&s("missing"));
    assert_eq!(contents(&m), HashMap::from([("b".into(), 2)]));
    unsafe { m.setObject_forKeyedSubscript(Some(&n(4)), ProtocolObject::from_ref(&*s("c"))) };
    unsafe { m.setObject_forKeyedSubscript(None, ProtocolObject::from_ref(&*s("b"))) };
    assert_eq!(contents(&m), HashMap::from([("c".into(), 4)]));
    m.addEntriesFromDictionary(&NSDictionary::from_retained_objects(&[&*s("d"), &*s("c")], &[n(5), n(6)]));
    assert_eq!(contents(&m), HashMap::from([("c".into(), 6), ("d".into(), 5)]));
    m.addEntriesFromDictionary(&m);
    m.setDictionary(&m);
    assert_eq!(m.count(), 2);
    m.removeObjectsForKeys(&NSArray::from_retained_slice(&[s("c"), s("zz")]));
    assert_eq!(contents(&m), HashMap::from([("d".into(), 5)]));
    m.setDictionary(&NSDictionary::from_retained_objects(&[&*s("x")], &[n(9)]));
    assert_eq!(contents(&m), HashMap::from([("x".into(), 9)]));
    m.removeAllObjects();
    assert_eq!(m.count(), 0);

    let made: Retained<NSMutableDictionary<NSString, NSNumber>> = NSMutableDictionary::dictionary();
    made.insert(&*s("k"), &n(1));
    assert!(made.isKindOfClass(NSMutableDictionary::<AnyObject, AnyObject>::class()));
    let sized = NSMutableDictionary::<NSString, NSNumber>::dictionaryWithCapacity(100);
    sized.insert(&*s("k"), &n(1));
    assert_eq!(sized.count(), 1);
}

/// Inserts and removals interleaved, against a Rust map: exercises the
/// table's growth and its removal of entries from the middle of runs.
#[test]
fn mutable_dictionaries_match_a_model() {
    let m = NSMutableDictionary::<NSString, NSNumber>::new();
    let mut model = HashMap::new();
    let mut x: u64 = 12345;
    for step in 0..4000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let key = format!("key-{}", x % 300);
        if x.is_multiple_of(3) {
            m.removeObjectForKey(&s(&key));
            model.remove(&key);
        } else {
            m.insert(&*s(&key), &n(step));
            model.insert(key, step);
        }
        if step % 500 == 0 {
            assert_eq!(contents(&m), model, "step {step}");
        }
    }
    assert_eq!(contents(&m), model);
    for (key, value) in &model {
        assert_eq!(m.objectForKey(&s(key)).unwrap().as_i64(), *value);
    }
}

#[test]
fn colliding_keys_survive_removal() {
    // Every key hashes alike, so the table's runs are as long as they get.
    let m = NSMutableDictionary::<Key, NSObject>::new();
    let values: Vec<_> = (0..40).map(|_| NSObject::new()).collect();
    for (i, value) in values.iter().enumerate() {
        m.insert(&*Key::new(i, 3), value);
    }
    for i in (0..40).step_by(3) {
        m.removeObjectForKey(&Key::new(i, 3));
    }
    for (i, value) in values.iter().enumerate() {
        let found = m.objectForKey(&Key::new(i, 3));
        if i % 3 == 0 {
            assert!(found.is_none(), "{i} was removed");
        } else {
            assert!(same(found, value), "{i} stays");
        }
    }
}

#[test]
fn keys_values_and_enumeration() {
    let dict = NSDictionary::from_retained_objects(&[&*s("a"), &*s("b"), &*s("c")], &[n(1), n(2), n(1)]);
    let mut keys: Vec<String> = dict.allKeys().iter().map(|k| k.to_string()).collect();
    keys.sort();
    assert_eq!(keys, ["a", "b", "c"]);
    let mut values: Vec<i64> = dict.allValues().iter().map(|v| v.as_i64()).collect();
    values.sort();
    assert_eq!(values, [1, 1, 2]);
    let mut ones: Vec<String> = dict.allKeysForObject(&n(1)).iter().map(|k| k.to_string()).collect();
    ones.sort();
    assert_eq!(ones, ["a", "c"]);
    let mut iterated: Vec<String> = dict.keys().map(|k| k.to_string()).collect();
    iterated.sort();
    assert_eq!(iterated, keys);
    let mut objects: Vec<i64> = dict.objects().map(|v| v.as_i64()).collect();
    objects.sort();
    assert_eq!(objects, values);
    let e = unsafe { dict.keyEnumerator() };
    let mut enumerated = Vec::new();
    while let Some(k) = e.nextObject() {
        enumerated.push(k.to_string());
    }
    enumerated.sort();
    assert_eq!(enumerated, keys);
    let found = dict.objectsForKeys_notFoundMarker(&NSArray::from_retained_slice(&[s("b"), s("zz")]), &n(0));
    assert_eq!(found.iter().map(|v| v.as_i64()).collect::<Vec<_>>(), [2, 0]);

    let seen = RefCell::new(Vec::new());
    let block = RcBlock::new(|k: NonNull<NSString>, v: NonNull<NSNumber>, _stop: NonNull<Bool>| {
        seen.borrow_mut().push((unsafe { k.as_ref() }.to_string(), unsafe { v.as_ref() }.as_i64()));
    });
    dict.enumerateKeysAndObjectsUsingBlock(&block);
    seen.borrow_mut().sort();
    assert_eq!(*seen.borrow(), [("a".into(), 1), ("b".into(), 2), ("c".into(), 1)]);
    let calls = Cell::new(0);
    let stopping = RcBlock::new(|_k: NonNull<NSString>, _v: NonNull<NSNumber>, stop: NonNull<Bool>| {
        calls.set(calls.get() + 1);
        unsafe { *stop.as_ptr() = Bool::YES };
    });
    dict.enumerateKeysAndObjectsUsingBlock(&stopping);
    assert_eq!(calls.get(), 1);

    let ones = RcBlock::new(|_k: NonNull<NSString>, v: NonNull<NSNumber>, _stop: NonNull<Bool>| {
        Bool::new(unsafe { v.as_ref() }.as_i64() == 1)
    });
    let keys_of_ones = dict.keysOfEntriesPassingTest(&ones);
    assert!(keys_of_ones.count() == 2 && keys_of_ones.containsObject(&s("a")) && keys_of_ones.containsObject(&s("c")));

    let by_value = RcBlock::new(|a: NonNull<AnyObject>, b: NonNull<AnyObject>| -> NSComparisonResult {
        let (a, b) = unsafe { (a.cast::<NSNumber>().as_ref(), b.cast::<NSNumber>().as_ref()) };
        a.compare(b)
    });
    let sorted = unsafe {
        NSDictionary::from_retained_objects(&[&*s("x"), &*s("y"), &*s("z")], &[n(3), n(1), n(2)])
            .keysSortedByValueUsingComparator(RcBlock::as_ptr(&by_value))
    };
    assert_eq!(sorted.iter().map(|k| k.to_string()).collect::<Vec<_>>(), ["y", "z", "x"]);

    // A mutable dictionary's block may change it.
    let m = NSMutableDictionary::from_retained_objects(&[&*s("a")], &[n(1)]);
    let adding = RcBlock::new(|_k: NonNull<NSString>, _v: NonNull<NSNumber>, _stop: NonNull<Bool>| {
        m.insert(&*s("b"), &n(2));
    });
    m.enumerateKeysAndObjectsUsingBlock(&adding);
    assert_eq!(m.count(), 2);
}

#[test]
fn equality_copies_and_hash() {
    let a = NSDictionary::from_retained_objects(&[&*s("a"), &*s("b")], &[n(1), n(2)]);
    let b = NSMutableDictionary::from_retained_objects(&[&*s("b"), &*s("a")], &[NSNumber::new_f64(2.0), n(1)]);
    assert!(a.isEqualToDictionary(&b) && b.isEqualToDictionary(&a));
    assert!(a.isEqual(Some(&b)));
    assert_eq!(a.hash(), b.hash());
    assert!(!a.isEqualToDictionary(&NSDictionary::from_retained_objects(&[&*s("a")], &[n(1)])));
    assert!(!a.isEqual(Some(&s("a"))));

    let frozen = b.copy();
    assert!(!frozen.isKindOfClass(NSMutableDictionary::<AnyObject, AnyObject>::class()));
    b.insert(&*s("c"), &n(3));
    assert_eq!((frozen.count(), b.count()), (2, 3));
    let thawed = a.mutableCopy();
    thawed.insert(&*s("z"), &n(26));
    assert_eq!((a.count(), thawed.count()), (2, 3));
    let copied = NSDictionary::dictionaryWithDictionary(&b);
    assert_eq!(copied.count(), 3);
}

#[test]
fn descriptions_match_foundation() {
    let keys = [s("zeta"), s("alpha"), s("hello world"), s("B"), s("_"), s("10"), s("9")];
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<_> = (1..=7).map(n).collect();
    let dict = NSDictionary::from_retained_objects(&key_refs, &values);
    assert_eq!(
        description(&dict),
        "{\n    10 = 6;\n    9 = 7;\n    B = 4;\n    \"_\" = 5;\n    alpha = 2;\n    \"hello world\" = 3;\n    zeta = 1;\n}"
    );
    assert_eq!(description(&NSDictionary::<NSString, NSNumber>::new()), "{\n}");
    assert_eq!(description(&NSMutableDictionary::<NSString, NSNumber>::new()), "{\n}");
    let nested: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_retained_objects(
        &[&*s("list"), &*s("map")],
        &[
            Retained::into_super(Retained::into_super(NSArray::from_retained_slice(&[s("x y")]))),
            Retained::into_super(Retained::into_super(NSDictionary::<NSString, NSNumber>::new())),
        ],
    );
    assert_eq!(description(&nested), "{\n    list =     (\n        \"x y\"\n    );\n    map =     {\n    };\n}");
}

#[test]
fn nil_fails_like_foundation() {
    let m = NSMutableDictionary::<NSString, NSNumber>::new();
    let reason = failure(|| unsafe {
        let _: () = msg_send![&*m, setObject: &*n(1), forKey: std::ptr::null::<AnyObject>()];
    });
    assert!(reason.ends_with("key cannot be nil"), "{reason}");
    let reason = failure(|| unsafe {
        let _: () = msg_send![&*m, setObject: std::ptr::null::<AnyObject>(), forKey: &*s("k")];
    });
    assert!(reason.ends_with("object cannot be nil (key: k)"), "{reason}");
    let reason = failure(|| unsafe {
        let _: () = msg_send![&*m, removeObjectForKey: std::ptr::null::<AnyObject>()];
    });
    assert!(reason.ends_with("key cannot be nil"), "{reason}");
    // Looking up nil finds nothing.
    let found: Option<Retained<AnyObject>> = unsafe { msg_send![&*m, objectForKey: std::ptr::null::<AnyObject>()] };
    assert!(found.is_none());
}

#[test]
fn mutation_during_iteration_is_detected() {
    let m = NSMutableDictionary::from_retained_objects(&[&*s("a"), &*s("b")], &[n(1), n(2)]);
    for change in [0, 1] {
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            for _ in m.keys() {
                // Even replacing a value counts.
                m.insert(&*s(if change == 0 { "c" } else { "a" }), &n(5));
            }
        }));
        assert!(result.is_err());
    }
}

#[test]
fn copies_are_independent() {
    let m = NSMutableDictionary::from_retained_objects(&[&*s("a"), &*s("b")], &[n(1), n(2)]);
    let frozen = m.copy();
    let thawed = m.mutableCopy();
    m.insert(&*s("c"), &n(3));
    thawed.removeObjectForKey(&s("a"));
    thawed.insert(&*s("b"), &n(20));
    assert_eq!(contents(&frozen), HashMap::from([("a".into(), 1), ("b".into(), 2)]));
    assert_eq!(contents(&m), HashMap::from([("a".into(), 1), ("b".into(), 2), ("c".into(), 3)]));
    assert_eq!(contents(&thawed), HashMap::from([("b".into(), 20)]));
    let again = frozen.mutableCopy();
    again.removeAllObjects();
    assert_eq!((again.count(), frozen.count()), (0, 2));
    let replaced = frozen.mutableCopy();
    replaced.setDictionary(&NSDictionary::from_retained_objects(&[&*s("z")], &[n(26)]));
    assert_eq!(contents(&frozen), HashMap::from([("a".into(), 1), ("b".into(), 2)]));
    drop(m);
    assert_eq!(frozen.objectForKey(&s("b")).unwrap().as_i64(), 2, "copies outlive the original");

    // A big dictionary: its copy shares the table, and a change to either
    // side after that leaves the other alone.
    let big = NSMutableDictionary::<NSString, NSNumber>::new();
    for i in 0..500 {
        big.insert(&*s(&format!("key-{i}")), &n(i));
    }
    let snapshot = big.copy();
    for i in (0..500).step_by(2) {
        big.removeObjectForKey(&s(&format!("key-{i}")));
    }
    assert_eq!((snapshot.count(), big.count()), (500, 250));
    assert!((0..500).all(|i| snapshot.objectForKey(&s(&format!("key-{i}"))).unwrap().as_i64() == i));

    // Retains and releases balance however the copies go.
    let value = NSObject::new();
    let before = value.retainCount();
    {
        let m = NSMutableDictionary::<NSString, NSObject>::new();
        m.insert(&*s("k"), &value);
        let copy = m.copy();
        let mutable_copy = m.mutableCopy();
        m.insert(&*s("j"), &value);
        mutable_copy.insert(&*s("k"), &value);
        drop((m, mutable_copy));
        assert_eq!(copy.count(), 1);
    }
    assert_eq!(value.retainCount(), before);
}

struct PairsIvars {
    keys: Vec<Retained<NSString>>,
    values: Vec<Retained<NSNumber>>,
}

define_class!(
    /// A dictionary defined outside the framework, through NSDictionary's
    /// primitive methods.
    #[unsafe(super(NSDictionary))]
    #[name = "ConformancePairs"]
    #[ivars = PairsIvars]
    struct Pairs;

    impl Pairs {
        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.ivars().keys.len()
        }

        #[unsafe(method(objectForKey:))]
        fn object_for_key(&self, key: &NSString) -> *mut NSNumber {
            let ivars = self.ivars();
            match ivars.keys.iter().position(|k| k.isEqualToString(key)) {
                Some(i) => Retained::as_ptr(&ivars.values[i]).cast_mut(),
                None => std::ptr::null_mut(),
            }
        }

        #[unsafe(method_id(keyEnumerator))]
        fn key_enumerator(&self) -> Retained<objc2_foundation::NSEnumerator<NSString>> {
            unsafe { NSArray::from_retained_slice(&self.ivars().keys).objectEnumerator() }
        }
    }
);

impl Pairs {
    fn new(pairs: &[(&str, i64)]) -> Retained<Self> {
        let ivars = PairsIvars {
            keys: pairs.iter().map(|(k, _)| s(k)).collect(),
            values: pairs.iter().map(|&(_, v)| n(v)).collect(),
        };
        let this = Self::alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }
}

#[test]
fn subclasses_get_the_rest_of_nsdictionary() {
    let pairs = Pairs::new(&[("b", 2), ("a", 1)]);
    let dict: &NSDictionary<NSString, NSNumber> =
        unsafe { &*(Retained::as_ptr(&pairs) as *const NSDictionary<NSString, NSNumber>) };
    assert_eq!(dict.count(), 2);
    assert_eq!(contents(dict), HashMap::from([("a".into(), 1), ("b".into(), 2)]));
    let mut keys: Vec<String> = dict.keys().map(|k| k.to_string()).collect();
    keys.sort();
    assert_eq!(keys, ["a", "b"]);
    let mut values: Vec<i64> = dict.allValues().iter().map(|v| v.as_i64()).collect();
    values.sort();
    assert_eq!(values, [1, 2]);
    assert_eq!(description(dict), "{\n    a = 1;\n    b = 2;\n}");
    let plain = NSDictionary::from_retained_objects(&[&*s("a"), &*s("b")], &[n(1), n(2)]);
    assert!(dict.isEqualToDictionary(&plain) && plain.isEqualToDictionary(dict));
    assert_eq!(contents(&dict.copy()), contents(&plain));
    let thawed = dict.mutableCopy();
    thawed.insert(&*s("c"), &n(3));
    assert_eq!(thawed.count(), 3);
}

/// Creation paths and accessors through objc2's bindings, so that debug
/// builds check each one's type encoding against the binding's.
#[test]
fn api_surface() {
    let d = unsafe {
        NSDictionary::<NSString, NSNumber>::dictionaryWithObject_forKey(&n(1), ProtocolObject::from_ref(&*s("a")))
    };
    assert_eq!(contents(&d), HashMap::from([("a".into(), 1)]));
    let keys: Retained<NSArray<ProtocolObject<dyn NSCopying>>> =
        unsafe { Retained::cast_unchecked(NSArray::from_retained_slice(&[s("x"), s("y")])) };
    let d = unsafe {
        NSDictionary::<NSString, NSNumber>::dictionaryWithObjects_forKeys(
            &NSArray::from_retained_slice(&[n(1), n(2)]),
            &keys,
        )
    };
    assert_eq!(contents(&d), HashMap::from([("x".into(), 1), ("y".into(), 2)]));
    let m = unsafe {
        NSMutableDictionary::<NSString, NSNumber>::initWithObjects_forKeys(
            NSMutableDictionary::alloc(),
            &NSArray::from_retained_slice(&[n(1), n(2)]),
            &keys,
        )
    };
    assert_eq!(m.count(), 2);
    let copied = unsafe { NSDictionary::initWithDictionary_copyItems(NSDictionary::alloc(), &d, true) };
    assert_eq!(contents(&copied), contents(&d));
    let copied = NSMutableDictionary::initWithDictionary(NSMutableDictionary::alloc(), &d);
    assert_eq!(contents(&copied), contents(&d));
    assert_eq!(d.objectForKeyedSubscript(&s("y")).unwrap().as_i64(), 2);
    assert_eq!(m.objectForKeyedSubscript(&s("y")).unwrap().as_i64(), 2);
    let mut objects = [std::ptr::NonNull::<NSNumber>::dangling(); 2];
    let mut ks = [std::ptr::NonNull::<NSString>::dangling(); 2];
    unsafe { d.getObjects_andKeys_count(objects.as_mut_ptr(), ks.as_mut_ptr(), 2) };
    let got: HashMap<String, i64> =
        ks.iter().zip(&objects).map(|(k, v)| unsafe { (k.as_ref().to_string(), v.as_ref().as_i64()) }).collect();
    assert_eq!(got, contents(&d));
    let text: Retained<NSString> = unsafe { d.descriptionWithLocale(None) };
    assert_eq!(text.to_string(), description(&d));
    let pairs = RefCell::new(0);
    let each =
        RcBlock::new(|_k: NonNull<NSString>, _v: NonNull<NSNumber>, _stop: NonNull<Bool>| *pairs.borrow_mut() += 1);
    d.enumerateKeysAndObjectsWithOptions_usingBlock(objc2_foundation::NSEnumerationOptions::Concurrent, &each);
    assert_eq!(*pairs.borrow(), 2);
    let all = RcBlock::new(|_k: NonNull<NSString>, _v: NonNull<NSNumber>, _stop: NonNull<Bool>| Bool::YES);
    assert_eq!(
        d.keysOfEntriesWithOptions_passingTest(objc2_foundation::NSEnumerationOptions::Concurrent, &all).count(),
        2
    );
    let sized = NSMutableDictionary::<NSString, NSNumber>::initWithCapacity(NSMutableDictionary::alloc(), 8);
    sized.setDictionary(&d);
    assert_eq!(contents(&sized), contents(&d));
}

#[test]
fn keys_sorted_by_value() {
    let d = NSDictionary::from_retained_objects(&[&*s("x"), &*s("y"), &*s("z")], &[n(3), n(1), n(2)]);
    let by_selector = unsafe { d.keysSortedByValueUsingSelector(objc2::sel!(compare:)) };
    assert_eq!(by_selector.iter().map(|k| k.to_string()).collect::<Vec<_>>(), ["y", "z", "x"]);
    let descending = RcBlock::new(|a: NonNull<AnyObject>, b: NonNull<AnyObject>| -> NSComparisonResult {
        let (a, b) = unsafe { (a.cast::<NSNumber>().as_ref(), b.cast::<NSNumber>().as_ref()) };
        b.compare(a)
    });
    let by_block = unsafe {
        d.keysSortedByValueWithOptions_usingComparator(
            objc2_foundation::NSSortOptions::Stable,
            RcBlock::as_ptr(&descending),
        )
    };
    assert_eq!(by_block.iter().map(|k| k.to_string()).collect::<Vec<_>>(), ["x", "z", "y"]);
}
