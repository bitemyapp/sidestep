//! `NSArray`, `NSMutableArray`, `NSSet`, `NSMutableSet`, `NSIndexSet`,
//! `NSMutableIndexSet`, `NSEnumerator` and `NSNull`, checked on macOS and on
//! Linux alike.

use std::cell::{Cell, RefCell};
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSBinarySearchingOptions, NSComparisonResult, NSCopying, NSEnumerationOptions, NSIndexSet, NSMutableArray,
    NSMutableCopying, NSMutableIndexSet, NSMutableSet, NSNotFound, NSNull, NSNumber, NSRange, NSSet, NSString,
    NSUInteger,
};

use sidestep as _;

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn n(value: i64) -> Retained<NSNumber> {
    NSNumber::new_i64(value)
}

fn strings(items: &[&str]) -> Retained<NSArray<NSString>> {
    NSArray::from_retained_slice(&items.iter().map(|t| s(t)).collect::<Vec<_>>())
}

fn numbers(items: &[i64]) -> Retained<NSArray<NSNumber>> {
    NSArray::from_retained_slice(&items.iter().map(|&v| n(v)).collect::<Vec<_>>())
}

fn values(array: &NSArray<NSNumber>) -> Vec<i64> {
    array.iter().map(|x| x.as_i64()).collect()
}

fn texts(array: &NSArray<NSString>) -> Vec<String> {
    array.iter().map(|x| x.to_string()).collect()
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

/// The message of a Rust panic, which objc2's iterators raise.
fn panic_message(f: impl FnOnce()) -> String {
    let e = std::panic::catch_unwind(AssertUnwindSafe(f)).expect_err("expected a panic");
    match e.downcast::<String>() {
        Ok(s) => *s,
        Err(e) => e.downcast::<&str>().map(|s| s.to_string()).unwrap_or_default(),
    }
}

#[test]
fn arrays_hold_their_elements() {
    let empty = NSArray::<NSNumber>::new();
    assert_eq!(empty.count(), 0);
    assert!(empty.firstObject().is_none() && empty.lastObject().is_none());
    assert!(empty.to_vec().is_empty());

    let objects: Vec<_> = (0..5).map(|_| NSObject::new()).collect();
    let array = NSArray::from_retained_slice(&objects);
    assert_eq!(array.count(), 5);
    for (i, obj) in objects.iter().enumerate() {
        assert!(std::ptr::eq(&*array.objectAtIndex(i), &**obj));
    }
    let refs: Vec<&NSObject> = objects.iter().map(|o| &**o).collect();
    let array = NSArray::from_slice(&refs);
    assert!(std::ptr::eq(&*array.firstObject().unwrap(), &*objects[0]));
    assert!(std::ptr::eq(&*array.lastObject().unwrap(), &*objects[4]));
    let back = array.to_vec();
    assert!(back.iter().zip(&objects).all(|(a, b)| std::ptr::eq(&**a, &**b)));
    let iterated: Vec<_> = array.iter().collect();
    assert!(iterated.iter().zip(&objects).all(|(a, b)| std::ptr::eq(&**a, &**b)));
    let into: Vec<_> = array.clone().into_iter().collect();
    assert_eq!(into.len(), 5);

    let big = numbers(&(0..1000).collect::<Vec<_>>());
    assert_eq!(values(&big), (0..1000).collect::<Vec<_>>());
    assert_eq!(big.objects_in_range(10..13).iter().map(|x| x.as_i64()).collect::<Vec<_>>(), [10, 11, 12]);
}

#[test]
fn arrays_retain_their_elements() {
    let obj = NSObject::new();
    let before = obj.retainCount();
    let array = NSArray::from_retained_slice(std::slice::from_ref(&obj));
    assert!(obj.retainCount() > before);
    drop(array);
    assert_eq!(obj.retainCount(), before);

    let mutable = NSMutableArray::<NSObject>::new();
    mutable.addObject(&obj);
    mutable.addObject(&obj);
    mutable.removeObjectAtIndex(0);
    mutable.removeAllObjects();
    assert_eq!(obj.retainCount(), before);
}

#[test]
fn searching_uses_is_equal() {
    let array = strings(&["alpha", "beta", "gamma", "beta"]);
    let beta = s("beta");
    assert!(array.containsObject(&beta));
    assert!(!array.containsObject(&s("delta")));
    assert_eq!(array.indexOfObject(&beta), 1);
    assert_eq!(array.indexOfObject(&s("delta")), NSNotFound as NSUInteger);
    assert_eq!(array.indexOfObject_inRange(&beta, NSRange::new(2, 2)), 3);
    // Identity, not equality. (Apple's short strings are tagged pointers,
    // so equal ones may be identical; plain objects never are.)
    let objects: Vec<_> = (0..3).map(|_| NSObject::new()).collect();
    let array = NSArray::from_retained_slice(&objects);
    assert_eq!(array.indexOfObjectIdenticalTo(&NSObject::new()), NSNotFound as NSUInteger);
    assert_eq!(array.indexOfObjectIdenticalTo(&objects[1]), 1);

    // Numbers compare by value across types.
    let nums = numbers(&[1, 2, 3]);
    assert!(nums.containsObject(&NSNumber::new_f64(2.0)));
    assert_eq!(nums.indexOfObject(&NSNumber::new_u8(3)), 2);
}

#[test]
fn equality_and_hash() {
    let a = strings(&["x", "y"]);
    let b = strings(&["x", "y"]);
    let c = strings(&["y", "x"]);
    let m = NSMutableArray::from_retained_slice(&[s("x"), s("y")]);
    assert!(a.isEqualToArray(&b));
    assert!(a.isEqual(Some(&b)));
    assert!(!a.isEqualToArray(&c));
    assert!(a.isEqual(Some(&m)) && m.isEqual(Some(&a)));
    assert_eq!(a.hash(), b.hash());
    assert_eq!(a.hash(), m.hash());
    assert!(!a.isEqual(Some(&s("x"))));
    assert!(!a.isEqual(Some(&NSSet::from_retained_slice(&[s("x"), s("y")]))));
    assert!(NSArray::<NSString>::new().isEqual(Some(&NSMutableArray::<NSString>::new())));
}

#[test]
fn derived_arrays() {
    let a = strings(&["a", "b", "c"]);
    assert_eq!(texts(&a.arrayByAddingObject(&s("d"))), ["a", "b", "c", "d"]);
    assert_eq!(texts(&a.arrayByAddingObjectsFromArray(&strings(&["e", "f"]))), ["a", "b", "c", "e", "f"]);
    assert_eq!(texts(&a.subarrayWithRange(NSRange::new(1, 2))), ["b", "c"]);
    assert_eq!(a.subarrayWithRange(NSRange::new(3, 0)).count(), 0);
    assert_eq!(a.componentsJoinedByString(&s(", ")).to_string(), "a, b, c");
    assert_eq!(NSArray::<NSString>::new().componentsJoinedByString(&s("-")).to_string(), "");
    assert_eq!(strings(&["é", "b"]).componentsJoinedByString(&s("→")).to_string(), "é→b");
    // Elements that aren't strings join by their descriptions.
    let mixed: Retained<NSArray<AnyObject>> = NSArray::from_retained_slice(&[
        Retained::into_super(Retained::into_super(Retained::into_super(n(1)))),
        Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_f64(2.5)))),
        Retained::into_super(Retained::into_super(numbers(&[3, 4]))),
    ]);
    assert_eq!(mixed.componentsJoinedByString(&s("|")).to_string(), "1|2.5|(\n    3,\n    4\n)");
    // Results of mutable arrays are immutable.
    let m = NSMutableArray::from_retained_slice(&[s("a")]);
    assert!(!m.arrayByAddingObject(&s("b")).isKindOfClass(NSMutableArray::<AnyObject>::class()));
    assert!(!m.subarrayWithRange(NSRange::new(0, 1)).isKindOfClass(NSMutableArray::<AnyObject>::class()));
}

#[test]
fn descriptions_match_foundation() {
    assert_eq!(description(&NSArray::<NSString>::new()), "(\n)");
    for (text, quoted) in [
        ("abc", "abc"),
        ("1abc", "1abc"),
        ("hello world", "\"hello world\""),
        ("", "\"\""),
        ("a_b", "\"a_b\""),
        ("a.b", "\"a.b\""),
        ("héllo", "\"h\\U00e9llo\""),
        ("🎉", "\"\\Ud83c\\Udf89\""),
        ("a\"b", "\"a\\\"b\""),
        ("a\\b", "\"a\\\\b\""),
        ("a\nb\tc", "\"a\\nb\\tc\""),
        ("\u{7}\u{8}\u{b}\u{c}", "\"\\a\\b\\v\\f\""),
        ("a\rb", "\"a\rb\""),
    ] {
        assert_eq!(description(&strings(&[text])), format!("(\n    {quoted}\n)"), "{text:?}");
    }
    let nested: Retained<NSArray<AnyObject>> = NSArray::from_retained_slice(&[
        Retained::into_super(Retained::into_super(Retained::into_super(n(1)))),
        Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_f64(2.5)))),
        Retained::into_super(Retained::into_super(numbers(&[3, 4]))),
        Retained::into_super(Retained::into_super(NSArray::<NSObject>::new())),
        Retained::into_super(Retained::into_super(NSNull::null())),
        Retained::into_super(Retained::into_super(NSSet::from_retained_slice(&[n(5)]))),
    ]);
    assert_eq!(
        description(&nested),
        "(\n    1,\n    \"2.5\",\n        (\n        3,\n        4\n    ),\n        (\n    ),\n    \"<null>\",\n    \"{(\\n    5\\n)}\"\n)"
    );
    let obj = NSObject::new();
    let with_object = NSArray::from_retained_slice(std::slice::from_ref(&obj));
    assert_eq!(description(&with_object), format!("(\n    \"<NSObject: {:p}>\"\n)", Retained::as_ptr(&obj)));
    let indented: Retained<NSString> =
        unsafe { msg_send![&*numbers(&[1]), descriptionWithLocale: None::<&AnyObject>, indent: 1usize] };
    assert_eq!(indented.to_string(), "    (\n        1\n    )");
    let m = NSMutableArray::from_retained_slice(&[s("x y")]);
    assert_eq!(description(&m), "(\n    \"x y\"\n)");
    let point = unsafe { objc2_foundation::NSValue::valueWithPoint(objc2_foundation::NSPoint::new(1.5, 2.0)) };
    assert_eq!(description(&NSArray::from_retained_slice(&[point])), "(\n    \"NSPoint: {1.5, 2}\"\n)");
}

#[test]
fn enumerators() {
    let a = numbers(&[1, 2, 3]);
    let e = unsafe { a.objectEnumerator() };
    assert_eq!(e.nextObject().map(|x| x.as_i64()), Some(1));
    assert_eq!(values(&e.allObjects()), [2, 3]);
    assert!(e.nextObject().is_none());
    let r = unsafe { a.reverseObjectEnumerator() };
    assert_eq!(r.iter().map(|x| x.as_i64()).collect::<Vec<_>>(), [3, 2, 1]);
    let empty = unsafe { NSArray::<NSNumber>::new().objectEnumerator() };
    assert!(empty.nextObject().is_none());
    assert_eq!(empty.allObjects().count(), 0);
}

#[test]
fn block_enumeration() {
    let a = numbers(&[10, 20, 30, 40]);
    let seen = RefCell::new(Vec::new());
    let block = RcBlock::new(|obj: NonNull<NSNumber>, i: NSUInteger, _stop: NonNull<Bool>| {
        seen.borrow_mut().push((unsafe { obj.as_ref() }.as_i64(), i));
    });
    a.enumerateObjectsUsingBlock(&block);
    assert_eq!(*seen.borrow(), [(10, 0), (20, 1), (30, 2), (40, 3)]);

    seen.borrow_mut().clear();
    a.enumerateObjectsWithOptions_usingBlock(NSEnumerationOptions::Reverse, &block);
    assert_eq!(*seen.borrow(), [(40, 3), (30, 2), (20, 1), (10, 0)]);

    let count = Cell::new(0);
    let stopping = RcBlock::new(|_obj: NonNull<NSNumber>, i: NSUInteger, stop: NonNull<Bool>| {
        count.set(count.get() + 1);
        if i == 1 {
            unsafe { *stop.as_ptr() = Bool::YES };
        }
    });
    a.enumerateObjectsUsingBlock(&stopping);
    assert_eq!(count.get(), 2);

    // A mutable array's block may change it without failing.
    let m = NSMutableArray::from_retained_slice(&[n(1), n(2), n(3)]);
    let adding = RcBlock::new(|_obj: NonNull<NSNumber>, i: NSUInteger, _stop: NonNull<Bool>| {
        if i == 0 {
            m.addObject(&n(9));
        }
    });
    m.enumerateObjectsUsingBlock(&adding);
    assert_eq!(values(&m), [1, 2, 3, 9]);
}

fn compare_numbers() -> RcBlock<dyn Fn(NonNull<AnyObject>, NonNull<AnyObject>) -> NSComparisonResult> {
    RcBlock::new(|a: NonNull<AnyObject>, b: NonNull<AnyObject>| {
        let (a, b) = unsafe { (a.cast::<NSNumber>().as_ref(), b.cast::<NSNumber>().as_ref()) };
        a.compare(b)
    })
}

#[test]
fn sorting() {
    let a = numbers(&[5, 3, 9, 1, 7, 3, 2, 8, 6, 4, 0, 11, 10]);
    let block = compare_numbers();
    let sorted = unsafe { a.sortedArrayUsingComparator(RcBlock::as_ptr(&block)) };
    assert_eq!(values(&sorted), [0, 1, 2, 3, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
    assert_eq!(values(&a)[0], 5, "the original is unchanged");

    let m = NSMutableArray::from_retained_slice(&a.to_vec());
    unsafe { m.sortUsingComparator(RcBlock::as_ptr(&block)) };
    assert_eq!(values(&m), values(&sorted));
    m.sort_by(|a, b| b.as_i64().cmp(&a.as_i64()));
    assert_eq!(values(&m), [11, 10, 9, 8, 7, 6, 5, 4, 3, 3, 2, 1, 0]);
    let one = numbers(&[1]);
    assert_eq!(values(&*unsafe { one.sortedArrayUsingComparator(RcBlock::as_ptr(&block)) }), [1]);
}

#[test]
fn binary_search() {
    let a = numbers(&[1, 3, 3, 3, 5, 7]);
    let block = compare_numbers();
    let search = |target: i64, location: usize, length: usize, options: NSBinarySearchingOptions| {
        let index = unsafe {
            a.indexOfObject_inSortedRange_options_usingComparator(
                &n(target),
                NSRange::new(location, length),
                options,
                RcBlock::as_ptr(&block),
            )
        };
        (index != NSNotFound as NSUInteger).then_some(index)
    };
    let (none, first, last, insert) = (
        NSBinarySearchingOptions(0),
        NSBinarySearchingOptions::FirstEqual,
        NSBinarySearchingOptions::LastEqual,
        NSBinarySearchingOptions::InsertionIndex,
    );
    // Without FirstEqual or LastEqual, any of the equal elements will do.
    assert!(matches!(search(3, 0, 6, none), Some(1..=3)));
    assert!(matches!(search(3, 0, 6, insert), Some(1..=3)));
    assert_eq!(search(3, 0, 6, first), Some(1));
    assert_eq!(search(3, 0, 6, last), Some(3));
    assert_eq!(search(3, 0, 6, insert | first), Some(1));
    assert_eq!(search(3, 0, 6, insert | last), Some(4));
    for target in [0, 4, 8] {
        assert_eq!(search(target, 0, 6, none), None);
        assert_eq!(search(target, 0, 6, first), None);
        assert_eq!(search(target, 0, 6, last), None);
    }
    assert_eq!(search(0, 0, 6, insert), Some(0));
    assert_eq!(search(4, 0, 6, insert | last), Some(4));
    assert_eq!(search(8, 0, 6, insert | first), Some(6));
    assert_eq!(search(7, 0, 6, insert | last), Some(6));
    assert_eq!(search(1, 0, 6, first), Some(0));
    assert_eq!(search(3, 2, 3, first), Some(2));
    assert_eq!(search(3, 2, 3, insert | last), Some(4));
    assert_eq!(search(9, 2, 0, insert), Some(2));
    assert_eq!(search(9, 2, 0, none), None);
    let reason = failure(|| {
        search(3, 0, 6, first | last);
    });
    assert!(
        reason.ends_with("both NSBinarySearchingFirstEqual and NSBinarySearchingLastEqual options cannot be specified")
    );
    assert!(
        failure(|| {
            search(3, 0, 9, none);
        })
        .ends_with("range {0, 9} extends beyond bounds [0 .. 5]")
    );
}

#[test]
fn mutable_arrays() {
    let m = NSMutableArray::<NSNumber>::new();
    m.addObject(&n(1));
    m.addObject(&n(3));
    m.insertObject_atIndex(&n(2), 1);
    m.insertObject_atIndex(&n(0), 0);
    m.insertObject_atIndex(&n(4), 4);
    assert_eq!(values(&m), [0, 1, 2, 3, 4]);
    m.removeObjectAtIndex(0);
    m.removeLastObject();
    assert_eq!(values(&m), [1, 2, 3]);
    m.replaceObjectAtIndex_withObject(1, &n(20));
    m.exchangeObjectAtIndex_withObjectAtIndex(0, 2);
    assert_eq!(values(&m), [3, 20, 1]);
    m.addObjectsFromArray(&numbers(&[1, 5]));
    m.removeObject(&NSNumber::new_f64(1.0));
    assert_eq!(values(&m), [3, 20, 5], "removeObject: removes every equal element");
    let objects: Vec<_> = (0..3).map(|_| NSObject::new()).collect();
    let plain = NSMutableArray::from_retained_slice(&objects);
    plain.addObject(&objects[1]);
    plain.removeObjectIdenticalTo(&NSObject::new());
    assert_eq!(plain.count(), 4, "identity, not equality");
    plain.removeObjectIdenticalTo(&objects[1]);
    assert_eq!(plain.count(), 2);
    m.removeObjectAtIndex(1);
    assert_eq!(values(&m), [3, 5]);
    m.setArray(&numbers(&[7, 8]));
    assert_eq!(values(&m), [7, 8]);
    m.addObjectsFromArray(&m.copy());
    m.addObjectsFromArray(&m);
    assert_eq!(values(&m), [7, 8, 7, 8, 7, 8, 7, 8]);
    m.setArray(&m);
    assert_eq!(m.count(), 8);
    m.removeAllObjects();
    assert_eq!(m.count(), 0);
    // Removing the last object of an empty array does nothing.
    m.removeLastObject();
    assert_eq!(m.count(), 0);

    let with_capacity = NSMutableArray::<NSNumber>::arrayWithCapacity(10);
    assert_eq!(with_capacity.count(), 0);
    with_capacity.addObject(&n(1));
    let from_class: Retained<NSMutableArray<NSNumber>> = NSMutableArray::array();
    from_class.addObject(&n(1));
    assert!(from_class.isKindOfClass(NSMutableArray::<AnyObject>::class()));
    let built = NSMutableArray::from_retained_slice(&[n(1), n(2)]);
    built.insert(2, &n(3));
    assert_eq!(values(&built), [1, 2, 3]);
}

#[test]
fn copies() {
    let a = numbers(&[1, 2]);
    let copy = a.copy();
    assert!(copy.isEqual(Some(&a)));
    let m = a.mutableCopy();
    assert!(m.isKindOfClass(NSMutableArray::<AnyObject>::class()));
    m.addObject(&n(3));
    assert_eq!(a.count(), 2);
    let frozen = m.copy();
    assert!(!frozen.isKindOfClass(NSMutableArray::<AnyObject>::class()));
    m.addObject(&n(4));
    assert_eq!(values(&frozen), [1, 2, 3]);
    assert_eq!(values(&m), [1, 2, 3, 4]);
    let class_copy = NSArray::arrayWithArray(&m);
    assert_eq!(values(&class_copy), [1, 2, 3, 4]);
}

#[test]
fn copies_are_independent() {
    let m = NSMutableArray::from_retained_slice(&[n(1), n(2), n(3)]);
    let frozen = m.copy();
    let thawed = m.mutableCopy();
    let again = m.copy();
    m.addObject(&n(4));
    thawed.removeObjectAtIndex(0);
    thawed.replaceObjectAtIndex_withObject(0, &n(20));
    assert_eq!(values(&frozen), [1, 2, 3]);
    assert_eq!(values(&again), [1, 2, 3]);
    assert_eq!(values(&m), [1, 2, 3, 4]);
    assert_eq!(values(&thawed), [20, 3]);

    let reordered = frozen.mutableCopy();
    reordered.sort_by(|a, b| b.as_i64().cmp(&a.as_i64()));
    assert_eq!(values(&reordered), [3, 2, 1]);
    assert_eq!(values(&frozen), [1, 2, 3]);
    let emptied = frozen.mutableCopy();
    emptied.removeAllObjects();
    assert_eq!((emptied.count(), frozen.count()), (0, 3));
    let replaced = frozen.mutableCopy();
    replaced.setArray(&numbers(&[9]));
    assert_eq!(values(&frozen), [1, 2, 3]);
    drop(m);
    assert_eq!(values(&frozen), [1, 2, 3], "copies outlive the original");

    // Retains and releases balance however the copies go.
    let obj = NSObject::new();
    let before = obj.retainCount();
    {
        let m = NSMutableArray::from_retained_slice(std::slice::from_ref(&obj));
        let copy = m.copy();
        let mutable_copy = m.mutableCopy();
        m.addObject(&obj);
        mutable_copy.addObject(&obj);
        drop((m, mutable_copy));
        assert_eq!(copy.count(), 1);
    }
    assert_eq!(obj.retainCount(), before);

    // Copies taken while the array is being enumerated.
    let m = NSMutableArray::from_retained_slice(&[n(1), n(2)]);
    let counts: Vec<usize> = m.iter().map(|_| m.copy().count()).collect();
    assert_eq!(counts, [2, 2]);
    let block = RcBlock::new(|_obj: NonNull<NSNumber>, _i: NSUInteger, _stop: NonNull<Bool>| {
        assert_eq!(m.copy().count(), 2);
    });
    m.enumerateObjectsUsingBlock(&block);
}

#[test]
fn out_of_range_fails_like_foundation() {
    let empty = NSArray::<NSNumber>::new();
    assert!(failure(|| drop(empty.objectAtIndex(0))).ends_with("index 0 beyond bounds for empty array"));
    let a = numbers(&[1, 2]);
    assert!(failure(|| drop(a.objectAtIndex(5))).ends_with("index 5 beyond bounds [0 .. 1]"));
    assert!(failure(|| drop(a.objectAtIndexedSubscript(2))).ends_with("index 2 beyond bounds [0 .. 1]"));
    assert!(
        failure(|| drop(a.subarrayWithRange(NSRange::new(1, 5))))
            .ends_with("range {1, 5} extends beyond bounds [0 .. 1]")
    );
    assert!(
        failure(|| {
            let _ = a.indexOfObject_inRange(&n(1), NSRange::new(1, 10));
        })
        .ends_with("range {1, 10} extends beyond bounds [0 .. 1]")
    );
    let m = NSMutableArray::from_retained_slice(&[n(1), n(2)]);
    assert!(failure(|| drop(m.objectAtIndex(5))).ends_with("index 5 beyond bounds [0 .. 1]"));
    assert!(failure(|| m.insertObject_atIndex(&n(9), 5)).ends_with("index 5 beyond bounds [0 .. 1]"));
    assert!(failure(|| m.removeObjectAtIndex(5)).ends_with("range {5, 1} extends beyond bounds [0 .. 1]"));
    assert!(failure(|| m.replaceObjectAtIndex_withObject(5, &n(9))).ends_with("index 5 beyond bounds [0 .. 1]"));
    assert!(failure(|| m.exchangeObjectAtIndex_withObjectAtIndex(0, 5)).ends_with("index 5 beyond bounds [0 .. 1]"));
    let empty_mutable = NSMutableArray::<NSNumber>::new();
    assert!(failure(|| drop(empty_mutable.objectAtIndex(0))).ends_with("index 0 beyond bounds for empty array"));
    assert_eq!(values(&m), [1, 2], "failed operations change nothing");
}

#[test]
fn nil_fails_like_foundation() {
    let m = NSMutableArray::<NSNumber>::new();
    let reason = failure(|| unsafe {
        let _: () = msg_send![&*m, addObject: std::ptr::null::<AnyObject>()];
    });
    assert!(reason.ends_with("object cannot be nil"), "{reason}");
    // Looking for nil finds nothing and removing it does nothing.
    let a = numbers(&[1]);
    let index: NSUInteger = unsafe { msg_send![&*a, indexOfObject: std::ptr::null::<AnyObject>()] };
    assert_eq!(index, NSNotFound as NSUInteger);
    let contains: bool = unsafe { msg_send![&*a, containsObject: std::ptr::null::<AnyObject>()] };
    assert!(!contains);
    let _: () = unsafe { msg_send![&*m, removeObject: std::ptr::null::<AnyObject>()] };
}

#[test]
fn mutation_during_iteration_is_detected() {
    let m = NSMutableArray::from_retained_slice(&[n(1), n(2), n(3)]);
    let message = panic_message(|| {
        for x in m.iter() {
            m.addObject(&x);
        }
    });
    assert!(message.contains("mutation"), "{message}");
    // Replacing an element is a mutation too.
    let message = panic_message(|| {
        for _ in m.iter() {
            m.replaceObjectAtIndex_withObject(0, &n(0));
        }
    });
    assert!(message.contains("mutation"), "{message}");
}

#[test]
fn null_is_a_singleton() {
    let a = NSNull::null();
    let b = NSNull::null();
    assert!(std::ptr::eq(&*a, &*b));
    assert!(std::ptr::eq(&*a, &*NSNull::new()));
    assert_eq!(description(&a), "<null>");
    assert!(a.isEqual(Some(&b)));
    let array: Retained<NSArray<NSNull>> = NSArray::from_retained_slice(&[a.clone(), b]);
    assert_eq!(array.count(), 2);
    assert!(array.containsObject(&a));
}

#[test]
fn sets_hold_distinct_members() {
    let set = NSSet::from_retained_slice(&[s("b"), s("a"), s("b"), s("c")]);
    assert_eq!(set.count(), 3);
    assert!(set.containsObject(&s("a")));
    assert!(!set.containsObject(&s("z")));
    let member = set.member(&s("a")).expect("member");
    assert_eq!(member.to_string(), "a");
    assert!(set.anyObject().is_some());
    assert!(NSSet::<NSString>::new().anyObject().is_none());
    let mut all: Vec<String> = set.allObjects().iter().map(|x| x.to_string()).collect();
    all.sort();
    assert_eq!(all, ["a", "b", "c"]);
    let mut iterated: Vec<String> = set.iter().map(|x| x.to_string()).collect();
    iterated.sort();
    assert_eq!(iterated, all);

    let from_array = NSSet::setWithArray(&numbers(&[1, 2, 2, 3]));
    assert_eq!(from_array.count(), 3);
    // 1 and 1.0 are the same member.
    assert!(from_array.containsObject(&NSNumber::new_f64(1.0)));
    let other = NSSet::from_retained_slice(&[n(3), n(2), NSNumber::new_f64(1.0)]);
    assert!(from_array.isEqualToSet(&other) && from_array.isEqual(Some(&other)));
    assert_eq!(from_array.hash(), other.hash());
    assert!(NSSet::from_retained_slice(&[n(1)]).isSubsetOfSet(&from_array));
    assert!(from_array.intersectsSet(&NSSet::from_retained_slice(&[n(3), n(9)])));
    assert!(!from_array.intersectsSet(&NSSet::from_retained_slice(&[n(9)])));
    assert_eq!(from_array.setByAddingObject(&n(4)).count(), 4);
    let big = RcBlock::new(|x: NonNull<NSNumber>, _stop: NonNull<Bool>| Bool::new(unsafe { x.as_ref() }.as_i64() > 1));
    let passing = from_array.objectsPassingTest(&big);
    assert!(passing.count() == 2 && passing.containsObject(&n(2)) && passing.containsObject(&n(3)));
    assert_eq!(from_array.setByAddingObjectsFromArray(&numbers(&[3, 4, 5])).count(), 5);

    assert_eq!(description(&NSSet::<NSString>::new()), "{(\n)}");
    assert_eq!(description(&NSSet::from_retained_slice(&[s("hello world")])), "{(\n    \"hello world\"\n)}");
    let with_array = NSSet::from_retained_slice(&[numbers(&[1, 2])]);
    assert_eq!(description(&with_array), "{(\n        (\n        1,\n        2\n    )\n)}");
}

#[test]
fn mutable_sets() {
    let m = NSMutableSet::<NSNumber>::new();
    m.addObject(&n(1));
    m.addObject(&n(2));
    m.addObject(&NSNumber::new_f64(2.0));
    assert_eq!(m.count(), 2);
    m.removeObject(&n(1));
    assert_eq!(m.count(), 1);
    m.addObjectsFromArray(&numbers(&[3, 4, 5]));
    m.unionSet(&NSSet::from_retained_slice(&[n(6)]));
    m.minusSet(&NSSet::from_retained_slice(&[n(2), n(3)]));
    let mut members: Vec<i64> = m.iter().map(|x| x.as_i64()).collect();
    members.sort();
    assert_eq!(members, [4, 5, 6]);
    m.intersectSet(&NSSet::from_retained_slice(&[n(5), n(6), n(7)]));
    let mut members: Vec<i64> = m.iter().map(|x| x.as_i64()).collect();
    members.sort();
    assert_eq!(members, [5, 6]);
    m.unionSet(&m);
    m.intersectSet(&m);
    assert_eq!(m.count(), 2);
    let frozen = m.copy();
    m.removeAllObjects();
    assert_eq!((m.count(), frozen.count()), (0, 2));
    assert!(!frozen.isKindOfClass(NSMutableSet::<AnyObject>::class()));
    let again = frozen.mutableCopy();
    again.addObject(&n(9));
    assert_eq!(again.count(), 3);
    let sized = NSMutableSet::<NSNumber>::setWithCapacity(100);
    for i in 0..100 {
        sized.addObject(&n(i));
    }
    for i in (0..100).step_by(2) {
        sized.removeObject(&n(i));
    }
    assert_eq!(sized.count(), 50);
    assert!((0..100).all(|i| sized.containsObject(&n(i)) == (i % 2 == 1)));
    m.minusSet(&m);
    let message = panic_message(|| {
        let m = NSMutableSet::from_retained_slice(&[n(1), n(2)]);
        for x in m.iter() {
            m.addObject(&n(x.as_i64() + 10));
        }
    });
    assert!(message.contains("mutation"), "{message}");
}

#[test]
fn make_objects_perform_selector() {
    let inner: Vec<Retained<NSMutableArray<NSNumber>>> = (0..3).map(|_| NSMutableArray::new()).collect();
    let outer = NSArray::from_retained_slice(&inner);
    unsafe { outer.makeObjectsPerformSelector_withObject(objc2::sel!(addObject:), Some(&n(7))) };
    assert!(inner.iter().all(|a| values(a) == [7]));
    let mutable_outer = NSMutableArray::from_retained_slice(&inner);
    unsafe { mutable_outer.makeObjectsPerformSelector(objc2::sel!(removeAllObjects)) };
    assert!(inner.iter().all(|a| a.count() == 0));
    let set = NSSet::from_retained_slice(&inner[..1]);
    unsafe { set.makeObjectsPerformSelector_withObject(objc2::sel!(addObject:), Some(&n(8))) };
    assert_eq!(values(&inner[0]), [8]);
}

#[test]
fn set_copies_are_independent() {
    let m = NSMutableSet::from_retained_slice(&[n(1), n(2)]);
    let frozen = m.copy();
    let thawed = m.mutableCopy();
    m.addObject(&n(3));
    thawed.removeObject(&n(1));
    let sorted = |set: &NSSet<NSNumber>| {
        let mut v: Vec<i64> = set.iter().map(|x| x.as_i64()).collect();
        v.sort();
        v
    };
    assert_eq!(sorted(&frozen), [1, 2]);
    assert_eq!(sorted(&m), [1, 2, 3]);
    assert_eq!(sorted(&thawed), [2]);
    let emptied = frozen.mutableCopy();
    emptied.removeAllObjects();
    assert_eq!((emptied.count(), frozen.count()), (0, 2));
    drop(m);
    assert!(frozen.containsObject(&n(2)));
}

#[test]
fn nil_set_members_fail_like_foundation() {
    let m = NSMutableSet::<NSNumber>::new();
    let reason = failure(|| unsafe {
        let _: () = msg_send![&*m, addObject: std::ptr::null::<AnyObject>()];
    });
    assert!(reason.ends_with("object cannot be nil"), "{reason}");
}

struct ListIvars {
    items: Vec<Retained<NSNumber>>,
}

define_class!(
    /// An array defined outside the framework, through NSArray's two
    /// primitive methods.
    #[unsafe(super(NSArray))]
    #[name = "ConformanceList"]
    #[ivars = ListIvars]
    struct List;

    impl List {
        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.ivars().items.len()
        }

        #[unsafe(method(objectAtIndex:))]
        fn object_at_index(&self, index: NSUInteger) -> *mut NSNumber {
            Retained::as_ptr(&self.ivars().items[index]).cast_mut()
        }
    }
);

impl List {
    fn new(items: &[i64]) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ListIvars { items: items.iter().map(|&v| n(v)).collect() });
        unsafe { msg_send![super(this), init] }
    }
}

#[test]
fn subclasses_get_the_rest_of_nsarray() {
    let list = List::new(&[3, 1, 2]);
    let array: &NSArray<NSNumber> = unsafe { &*(Retained::as_ptr(&list) as *const NSArray<NSNumber>) };
    assert_eq!(array.count(), 3);
    assert_eq!(values(array), [3, 1, 2]);
    assert!(array.containsObject(&n(1)));
    assert_eq!(array.indexOfObject(&n(2)), 2);
    assert!(array.isEqualToArray(&numbers(&[3, 1, 2])));
    assert!(numbers(&[3, 1, 2]).isEqualToArray(array));
    assert_eq!(array.lastObject().map(|x| x.as_i64()), Some(2));
    assert_eq!(description(array), "(\n    3,\n    1,\n    2\n)");
    assert_eq!(values(&array.copy()), [3, 1, 2]);
    let block = compare_numbers();
    assert_eq!(values(&*unsafe { array.sortedArrayUsingComparator(RcBlock::as_ptr(&block)) }), [1, 2, 3]);
    let e = unsafe { array.reverseObjectEnumerator() };
    assert_eq!(values(&e.allObjects()), [2, 1, 3]);
}

fn indexes(set: &NSIndexSet) -> Vec<usize> {
    let seen = RefCell::new(Vec::new());
    let block = RcBlock::new(|i: NSUInteger, _stop: NonNull<Bool>| seen.borrow_mut().push(i));
    set.enumerateIndexesUsingBlock(&block);
    drop(block);
    seen.into_inner()
}

#[test]
fn index_sets() {
    let empty = NSIndexSet::new();
    assert_eq!(empty.count(), 0);
    assert_eq!(empty.firstIndex(), NSNotFound as NSUInteger);
    assert_eq!(empty.lastIndex(), NSNotFound as NSUInteger);
    assert!(description(&empty).ends_with("(no indexes)"));

    let m = NSMutableIndexSet::new();
    m.addIndex(5);
    m.addIndexesInRange(NSRange::new(1, 2));
    m.addIndex(3);
    m.addIndex(10);
    assert_eq!(indexes(&m), [1, 2, 3, 5, 10]);
    assert_eq!(m.count(), 5);
    assert!(description(&m).ends_with("[number of indexes: 5 (in 3 ranges), indexes: (1-3 5 10)]"));
    assert_eq!((m.firstIndex(), m.lastIndex()), (1, 10));
    assert_eq!(m.indexGreaterThanIndex(3), 5);
    assert_eq!(m.indexGreaterThanOrEqualToIndex(4), 5);
    assert_eq!(m.indexLessThanIndex(1), NSNotFound as NSUInteger);
    assert_eq!(m.indexLessThanOrEqualToIndex(4), 3);
    assert_eq!(m.indexGreaterThanIndex(10), NSNotFound as NSUInteger);
    assert_eq!(m.countOfIndexesInRange(NSRange::new(2, 4)), 3);
    assert!(m.containsIndex(2) && !m.containsIndex(4) && !m.containsIndex(NSNotFound as NSUInteger));
    assert!(m.containsIndexesInRange(NSRange::new(1, 3)) && !m.containsIndexesInRange(NSRange::new(1, 5)));
    assert!(m.intersectsIndexesInRange(NSRange::new(4, 2)) && !m.intersectsIndexesInRange(NSRange::new(6, 4)));
    assert!(m.containsIndexes(&NSIndexSet::indexSetWithIndexesInRange(NSRange::new(1, 2))));

    let copy = m.copy();
    assert!(copy.isEqualToIndexSet(&m) && m.isEqual(Some(&copy)));
    assert_eq!(copy.hash(), m.hash());
    m.removeIndex(2);
    m.removeIndexesInRange(NSRange::new(9, 5));
    assert_eq!(indexes(&m), [1, 3, 5]);
    assert_eq!(indexes(&copy), [1, 2, 3, 5, 10]);
    m.addIndexes(&copy);
    m.removeIndexes(&NSIndexSet::indexSetWithIndex(3));
    assert_eq!(indexes(&m), [1, 2, 5, 10]);
    m.removeAllIndexes();
    assert_eq!(m.count(), 0);

    let shifted = NSMutableIndexSet::indexSetWithIndexesInRange(NSRange::new(0, 10));
    shifted.shiftIndexesStartingAtIndex_by(5, -2);
    assert_eq!(indexes(&shifted), (0..8).collect::<Vec<_>>());
    let shifted = NSMutableIndexSet::indexSetWithIndexesInRange(NSRange::new(0, 3));
    shifted.addIndex(7);
    shifted.shiftIndexesStartingAtIndex_by(1, 3);
    assert_eq!(indexes(&shifted), [0, 4, 5, 10]);
    let shifted = NSMutableIndexSet::indexSetWithIndex(2);
    shifted.shiftIndexesStartingAtIndex_by(1, -5);
    assert_eq!(shifted.count(), 0);

    let big = NSIndexSet::indexSetWithIndexesInRange(NSRange::new(0, 1_000_000));
    assert_eq!(big.count(), 1_000_000);
    assert!(description(&big).ends_with("[number of indexes: 1000000 (in 1 ranges), indexes: (0-999999)]"));
    let ranges = RefCell::new(Vec::new());
    let block = RcBlock::new(|r: NSRange, _stop: NonNull<Bool>| ranges.borrow_mut().push((r.location, r.length)));
    copy.enumerateRangesUsingBlock(&block);
    assert_eq!(*ranges.borrow(), [(1, 3), (5, 1), (10, 1)]);
    let even = RcBlock::new(|i: NSUInteger, _stop: NonNull<Bool>| Bool::new(i.is_multiple_of(2)));
    assert_eq!(indexes(&copy.indexesPassingTest(&even)), [2, 10]);
    assert_eq!(copy.indexPassingTest(&even), 2);
    let mut buffer = [0usize; 3];
    let mut range = NSRange::new(0, 20);
    let n = unsafe { copy.getIndexes_maxCount_inIndexRange(NonNull::from(&mut buffer).cast(), 3, &mut range) };
    assert_eq!((n, buffer), (3, [1, 2, 3]));
    assert_eq!(range, NSRange::new(4, 16), "what is left starts after the last index given");

    let reason = failure(|| NSMutableIndexSet::new().addIndex(NSNotFound as NSUInteger));
    assert!(reason.ends_with("exceeds maximum index value of NSNotFound - 1"), "{reason}");
}

#[test]
fn arrays_with_index_sets() {
    let a = numbers(&[0, 1, 2, 3, 4, 5]);
    let chosen = NSMutableIndexSet::indexSetWithIndexesInRange(NSRange::new(1, 3));
    chosen.addIndex(5);
    assert_eq!(values(&a.objectsAtIndexes(&chosen)), [1, 2, 3, 5]);
    let too_far = NSIndexSet::indexSetWithIndex(9);
    assert!(failure(|| drop(a.objectsAtIndexes(&too_far))).ends_with("index 9 in index set beyond bounds [0 .. 5]"));
    let even = RcBlock::new(|x: NonNull<NSNumber>, _i: NSUInteger, _stop: NonNull<Bool>| {
        Bool::new(unsafe { x.as_ref() }.as_i64() % 2 == 0)
    });
    assert_eq!(indexes(&a.indexesOfObjectsPassingTest(&even)), [0, 2, 4]);
    assert_eq!(a.indexOfObjectWithOptions_passingTest(NSEnumerationOptions::Reverse, &even), 4);
    let seen = RefCell::new(Vec::new());
    let block = RcBlock::new(|x: NonNull<NSNumber>, i: NSUInteger, _stop: NonNull<Bool>| {
        seen.borrow_mut().push((unsafe { x.as_ref() }.as_i64(), i));
    });
    a.enumerateObjectsAtIndexes_options_usingBlock(&chosen, NSEnumerationOptions(0), &block);
    assert_eq!(*seen.borrow(), [(1, 1), (2, 2), (3, 3), (5, 5)]);

    let m = a.mutableCopy();
    m.removeObjectsAtIndexes(&chosen);
    assert_eq!(values(&m), [0, 4]);
    let places = NSMutableIndexSet::indexSetWithIndex(0);
    places.addIndex(3);
    m.insertObjects_atIndexes(&numbers(&[100, 200]), &places);
    assert_eq!(values(&m), [100, 0, 4, 200]);
    m.replaceObjectsAtIndexes_withObjects(&places, &numbers(&[7, 8]));
    assert_eq!(values(&m), [7, 0, 4, 8]);
    let reason = failure(|| m.insertObjects_atIndexes(&numbers(&[1]), &places));
    assert!(reason.ends_with("count of array (1) differs from count of index set (2)"), "{reason}");
    let reason = failure(|| m.removeObjectsAtIndexes(&too_far));
    assert!(reason.ends_with("index 9 in index set beyond bounds [0 .. 3]"), "{reason}");
    assert_eq!(values(&m), [7, 0, 4, 8]);
}

/// Every method implemented, through objc2's bindings, so that debug builds
/// check each one's type encoding against the binding's.
#[test]
fn api_surface() {
    let a = numbers(&[3, 1, 2]);
    let mut out = [std::ptr::NonNull::<NSNumber>::dangling(); 2];
    unsafe { a.getObjects_range(NonNull::from(&mut out).cast(), NSRange::new(1, 2)) };
    assert_eq!(unsafe { (out[0].as_ref().as_i64(), out[1].as_ref().as_i64()) }, (1, 2));
    unsafe extern "C-unwind" fn by_value(
        a: NonNull<NSNumber>,
        b: NonNull<NSNumber>,
        _: *mut std::ffi::c_void,
    ) -> isize {
        unsafe { a.as_ref().compare(b.as_ref()) as isize }
    }
    assert_eq!(values(&*unsafe { a.sortedArrayUsingFunction_context(by_value, std::ptr::null_mut()) }), [1, 2, 3]);
    assert_eq!(a.indexOfObjectIdenticalTo_inRange(&a.objectAtIndex(2), NSRange::new(1, 2)), 2);
    assert_eq!(a.firstObjectCommonWithArray(&numbers(&[9, 2])).map(|x| x.as_i64()), Some(2));
    assert_eq!(NSArray::arrayWithObject(&*n(4)).count(), 1);
    assert_eq!(values(&NSArray::arrayWithArray(&a)), [3, 1, 2]);
    assert_eq!(values(&*unsafe { NSArray::initWithArray_copyItems(NSArray::alloc(), &a, true) }), [3, 1, 2]);
    assert_eq!(values(&NSArray::initWithArray(NSArray::alloc(), &a)), [3, 1, 2]);
    let d: Retained<NSString> = unsafe { a.descriptionWithLocale(None) };
    assert_eq!(d.to_string(), description(&a));
    let block = compare_numbers();
    assert_eq!(
        values(&*unsafe {
            a.sortedArrayWithOptions_usingComparator(objc2_foundation::NSSortOptions::Stable, RcBlock::as_ptr(&block))
        }),
        [1, 2, 3]
    );
    let odd = RcBlock::new(|x: NonNull<NSNumber>, _i: NSUInteger, _stop: NonNull<Bool>| {
        Bool::new(unsafe { x.as_ref() }.as_i64() % 2 == 1)
    });
    assert_eq!(a.indexOfObjectPassingTest(&odd), 0);

    let m = NSMutableArray::from_retained_slice(&[n(1), n(2), n(1), n(3), n(1)]);
    m.removeObject_inRange(&n(1), NSRange::new(1, 3));
    assert_eq!(values(&m), [1, 2, 3, 1]);
    let objects: Vec<_> = (0..2).map(|_| NSObject::new()).collect();
    let plain = NSMutableArray::from_retained_slice(&[objects[0].clone(), objects[1].clone(), objects[0].clone()]);
    plain.removeObjectIdenticalTo_inRange(&objects[0], NSRange::new(1, 2));
    assert_eq!(plain.count(), 2);
    m.removeObjectsInRange(NSRange::new(1, 2));
    assert_eq!(values(&m), [1, 1]);
    m.replaceObjectsInRange_withObjectsFromArray(NSRange::new(0, 1), &numbers(&[7, 8]));
    assert_eq!(values(&m), [7, 8, 1]);
    m.removeObjectsInArray(&numbers(&[8]));
    assert_eq!(values(&m), [7, 1]);
    m.setObject_atIndexedSubscript(&n(5), 2);
    m.setObject_atIndexedSubscript(&n(6), 0);
    assert_eq!(values(&m), [6, 1, 5]);
    unsafe { m.sortWithOptions_usingComparator(objc2_foundation::NSSortOptions::Stable, RcBlock::as_ptr(&block)) };
    unsafe { m.sortUsingFunction_context(by_value, std::ptr::null_mut()) };
    assert_eq!(values(&m), [1, 5, 6]);
    let copied = unsafe { NSMutableArray::initWithArray_copyItems(NSMutableArray::alloc(), &m, false) };
    assert_eq!(values(&copied), [1, 5, 6]);
    let e = unsafe { m.objectEnumerator() };
    assert_eq!(e.iter().map(|x| x.as_i64()).collect::<Vec<_>>(), [1, 5, 6]);

    let s = NSSet::from_retained_slice(&[n(1), n(2)]);
    assert_eq!(NSSet::setWithSet(&s).count(), 2);
    assert_eq!(unsafe { NSSet::initWithSet_copyItems(NSSet::alloc(), &s, true) }.count(), 2);
    assert_eq!(NSSet::initWithArray(NSSet::alloc(), &numbers(&[1, 1])).count(), 1);
    assert_eq!(s.setByAddingObjectsFromSet(&NSSet::from_retained_slice(&[n(3)])).count(), 3);
    assert_eq!(NSSet::setWithObject(&*n(1)).count(), 1);
    let ms = NSMutableSet::setWithArray(&numbers(&[1, 2]));
    ms.setSet(&NSSet::from_retained_slice(&[n(7)]));
    assert_eq!(ms.count(), 1);
    let members = RefCell::new(0);
    let each = RcBlock::new(|_x: NonNull<NSNumber>, _stop: NonNull<Bool>| *members.borrow_mut() += 1);
    s.enumerateObjectsUsingBlock(&each);
    s.enumerateObjectsWithOptions_usingBlock(NSEnumerationOptions::Concurrent, &each);
    assert_eq!(*members.borrow(), 4);
    let e = unsafe { s.objectEnumerator() };
    assert_eq!(e.allObjects().count(), 2);
    let d: Retained<NSString> = unsafe { s.descriptionWithLocale(None) };
    assert!(d.to_string().starts_with("{("));
}

/// Hands an object to another thread. Immutable collections and numbers
/// are thread-safe in Foundation, though objc2's types don't say so.
struct Shared<T>(T);
unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}

#[test]
fn immutable_collections_cross_threads() {
    let items: Vec<_> = (0..200).map(|i| s(&format!("element-{i}-long-enough"))).collect();
    let array = Shared(NSArray::from_retained_slice(&items));
    let set = Shared(NSSet::from_retained_slice(&items));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let (array, set) = (&array, &set);
            scope.spawn(move || {
                for i in 0..200 {
                    let probe = s(&format!("element-{i}-long-enough"));
                    assert_eq!(array.0.indexOfObject(&probe), i);
                    assert!(set.0.containsObject(&probe));
                    assert_eq!(NSNumber::new_i64(i as i64 % 50).as_i64(), i as i64 % 50);
                }
                assert_eq!(array.0.iter().count(), 200);
            });
        }
    });

    // Copies of a mutable array released on other threads, while the
    // original changes on this one.
    let m = NSMutableArray::from_retained_slice(&items);
    for (round, item) in items.iter().take(20).enumerate() {
        let copy = Shared(m.copy());
        let reader = std::thread::spawn(move || {
            let copy = copy;
            assert_eq!(copy.0.count(), 200 + round);
            drop(copy);
        });
        m.addObject(item);
        reader.join().unwrap();
    }
    assert_eq!(m.count(), 220);
}
