//! `NSOrderedSet`, `NSMutableOrderedSet`, `NSSortDescriptor`, `NSHashTable`,
//! `NSMapTable`, `NSPointerArray`, `NSCountedSet` and `NSCache`, checked on
//! macOS and on Linux alike.

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_foundation::{
    NSArray, NSBinarySearchingOptions, NSCache, NSCacheDelegate, NSCopying, NSCountedSet, NSDictionary,
    NSEnumerationOptions, NSFastEnumeration, NSFastEnumerationState, NSHashTable, NSIndexSet, NSMapTable,
    NSMutableArray, NSMutableCopying, NSMutableIndexSet, NSMutableOrderedSet, NSMutableSet, NSNotFound, NSNumber,
    NSOrderedSet, NSPointerArray, NSPointerFunctions, NSRange, NSSet, NSSortDescriptor, NSSortOptions, NSString,
    NSUInteger,
};

use sidestep as _;

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn n(value: i64) -> Retained<NSNumber> {
    NSNumber::new_i64(value)
}

fn numbers(items: &[i64]) -> Retained<NSArray<NSNumber>> {
    NSArray::from_retained_slice(&items.iter().map(|&v| n(v)).collect::<Vec<_>>())
}

fn values(array: &NSArray<NSNumber>) -> Vec<i64> {
    array.iter().map(|x| x.as_i64()).collect()
}

fn ordered(items: &[i64]) -> Retained<NSOrderedSet<NSNumber>> {
    NSOrderedSet::orderedSetWithArray(&numbers(items))
}

fn mutable_ordered(items: &[i64]) -> Retained<NSMutableOrderedSet<NSNumber>> {
    NSMutableOrderedSet::orderedSetWithArray(&numbers(items))
}

/// An ordered set's members, through `-count` and `-objectAtIndex:`.
fn members(set: &NSOrderedSet<NSNumber>) -> Vec<i64> {
    (0..set.count()).map(|i| set.objectAtIndex(i).as_i64()).collect()
}

fn indexes(items: &[usize]) -> Retained<NSIndexSet> {
    let set = NSMutableIndexSet::new();
    for &i in items {
        set.addIndex(i);
    }
    Retained::into_super(set)
}

fn description(obj: &AnyObject) -> String {
    let d: Retained<NSString> = unsafe { msg_send![obj, description] };
    d.to_string()
}

fn is_kind(obj: &AnyObject, class: &objc2::runtime::AnyClass) -> bool {
    unsafe { msg_send![obj, isKindOfClass: class] }
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

/// Everything fast enumeration hands out, batch by batch, with a check
/// that the mutation count stays put while nothing changes.
fn fast_enumerate<T: NSFastEnumeration + Message>(collection: &T) -> Vec<*mut AnyObject> {
    let mut state = NSFastEnumerationState {
        state: 0,
        itemsPtr: std::ptr::null_mut(),
        mutationsPtr: std::ptr::null_mut(),
        extra: [0; 5],
    };
    let mut buffer = [std::ptr::null_mut::<AnyObject>(); 4];
    let mut out = Vec::new();
    let mut mutations = None;
    loop {
        let got = unsafe {
            collection.countByEnumeratingWithState_objects_count(
                NonNull::from(&mut state),
                NonNull::new(buffer.as_mut_ptr()).unwrap(),
                buffer.len(),
            )
        };
        if got == 0 {
            return out;
        }
        let now = unsafe { *state.mutationsPtr };
        assert_eq!(*mutations.get_or_insert(now), now, "nothing changed the collection");
        for i in 0..got {
            out.push(unsafe { *state.itemsPtr.add(i) });
        }
    }
}

fn fast_numbers<T: NSFastEnumeration + Message>(collection: &T) -> Vec<i64> {
    fast_enumerate(collection).into_iter().map(|p| unsafe { &*p.cast::<NSNumber>() }.as_i64()).collect()
}

type Compare = RcBlock<dyn Fn(NonNull<AnyObject>, NonNull<AnyObject>) -> objc2_foundation::NSComparisonResult>;

fn compare_numbers() -> Compare {
    RcBlock::new(|a: NonNull<AnyObject>, b: NonNull<AnyObject>| {
        let (a, b) = unsafe { (a.cast::<NSNumber>().as_ref(), b.cast::<NSNumber>().as_ref()) };
        a.compare(b)
    })
}

// NSOrderedSet

#[test]
fn ordered_sets_keep_order_and_distinct_members() {
    let set = ordered(&[3, 1, 3, 2, 1]);
    assert_eq!(members(&set), [3, 1, 2]);
    assert_eq!(set.count(), 3);
    assert_eq!(set.indexOfObject(&n(2)), 2);
    assert_eq!(set.indexOfObject(&n(9)), NSNotFound as NSUInteger);
    // Equality, not identity, and numbers across types.
    assert!(set.containsObject(&NSNumber::new_f64(1.0)));
    assert!(!set.containsObject(&n(4)));
    assert_eq!(set.firstObject().unwrap().as_i64(), 3);
    assert_eq!(set.lastObject().unwrap().as_i64(), 2);
    assert!(NSOrderedSet::<NSNumber>::new().firstObject().is_none());
    assert_eq!(set.objectAtIndexedSubscript(1).as_i64(), 1);
    assert_eq!(values(&set.objectsAtIndexes(&indexes(&[0, 2]))), [3, 2]);
    assert_eq!(members(&set.reversedOrderedSet()), [2, 1, 3]);

    // Large sets find members by hash.
    let big: Vec<i64> = (0..1000).rev().collect();
    let big = ordered(&big);
    assert_eq!(big.count(), 1000);
    for v in [0, 1, 500, 999] {
        assert_eq!(big.indexOfObject(&n(v)), (999 - v) as usize);
    }
    let strings = NSOrderedSet::orderedSetWithArray(&NSArray::from_retained_slice(&[s("a"), s("b"), s("a")]));
    assert_eq!(strings.count(), 2);
    assert_eq!(strings.indexOfObject(&s("b")), 1);

    // Objects, by -isEqual: and -hash.
    let objects: Vec<_> = (0..20).map(|_| NSObject::new()).collect();
    let set = NSOrderedSet::orderedSetWithArray(&NSArray::from_retained_slice(&objects));
    for (i, o) in objects.iter().enumerate() {
        assert_eq!(set.indexOfObject(o), i);
    }
    assert_eq!(set.indexOfObject(&NSObject::new()), NSNotFound as NSUInteger);
}

#[test]
fn ordered_set_constructors() {
    let array = numbers(&[5, 6, 5, 7]);
    let from_array = NSOrderedSet::orderedSetWithArray(&array);
    assert_eq!(members(&from_array), [5, 6, 7]);
    let range = unsafe { NSOrderedSet::orderedSetWithArray_range_copyItems(&array, NSRange::new(1, 3), false) };
    assert_eq!(members(&range), [6, 5, 7]);
    let from_set = NSOrderedSet::orderedSetWithSet(&NSSet::from_retained_slice(&[n(1)]));
    assert_eq!(members(&from_set), [1]);
    let copy = NSOrderedSet::orderedSetWithOrderedSet(&from_array);
    assert_eq!(members(&copy), [5, 6, 7]);
    let part = unsafe { NSOrderedSet::orderedSetWithOrderedSet_range_copyItems(&from_array, NSRange::new(1, 2), true) };
    assert_eq!(members(&part), [6, 7]);
    let one = NSOrderedSet::orderedSetWithObject(&*n(4));
    assert_eq!(members(&one), [4]);
    let init = NSOrderedSet::initWithArray(NSOrderedSet::alloc(), &array);
    assert_eq!(members(&init), [5, 6, 7]);
    let init = NSOrderedSet::initWithOrderedSet(NSOrderedSet::alloc(), &init);
    assert_eq!(members(&init), [5, 6, 7]);
    let init = NSOrderedSet::initWithObject(NSOrderedSet::alloc(), &*n(8));
    assert_eq!(members(&init), [8]);
    let objects = [n(1), n(2), n(1)];
    let ptrs: Vec<_> = objects.iter().map(|o| NonNull::from(&**o)).collect();
    let init = unsafe { NSOrderedSet::initWithObjects_count(NSOrderedSet::alloc(), ptrs.as_ptr().cast_mut(), 3) };
    assert_eq!(members(&init), [1, 2]);
    let mutable = NSMutableOrderedSet::orderedSetWithArray(&array);
    assert!(mutable.isKindOfClass(NSMutableOrderedSet::<AnyObject>::class()));
    let mutable = NSMutableOrderedSet::initWithArray(NSMutableOrderedSet::alloc(), &array);
    mutable.addObject(&n(1));
    assert_eq!(members(&mutable), [5, 6, 7, 1]);
    let with_capacity = NSMutableOrderedSet::<NSNumber>::orderedSetWithCapacity(10);
    assert_eq!(with_capacity.count(), 0);
}

#[test]
fn ordered_set_equality_copies_and_descriptions() {
    let a = ordered(&[1, 2, 3]);
    let m = mutable_ordered(&[1, 2, 3]);
    assert!(a.isEqual(Some(&m)) && m.isEqual(Some(&a)));
    assert!(a.isEqualToOrderedSet(&m));
    assert!(!a.isEqual(Some(&ordered(&[3, 2, 1]))), "order matters");
    assert!(!a.isEqual(Some(&numbers(&[1, 2, 3]))), "an array isn't an ordered set");
    assert!(!a.isEqual(None));
    assert_eq!(a.hash(), 3);
    assert_eq!(NSOrderedSet::<NSNumber>::new().hash(), 0);

    assert!(std::ptr::eq(&*a.copy(), &*a), "immutable copies are the same object");
    let frozen = m.copy();
    assert!(!frozen.isKindOfClass(NSMutableOrderedSet::<AnyObject>::class()));
    m.addObject(&n(4));
    assert_eq!(members(&frozen), [1, 2, 3], "copies don't follow changes");
    let thawed = a.mutableCopy();
    thawed.addObject(&n(9));
    assert_eq!(members(&a), [1, 2, 3]);
    assert_eq!(members(&thawed), [1, 2, 3, 9]);

    assert_eq!(description(&a), "{(\n    1,\n    2,\n    3\n)}");
    assert_eq!(description(&NSOrderedSet::<NSNumber>::new()), "{(\n)}");
    let words = NSOrderedSet::orderedSetWithArray(&NSArray::from_retained_slice(&[s("a b"), s("c")]));
    assert_eq!(description(&words), "{(\n    \"a b\",\n    c\n)}");
    let indented: Retained<NSString> =
        unsafe { msg_send![&*a, descriptionWithLocale: None::<&AnyObject>, indent: 1usize] };
    assert_eq!(indented.to_string(), "    {(\n        1,\n        2,\n        3\n    )}");
    // In an array or a dictionary, an ordered set is quoted, as a set is.
    let nested: Retained<NSArray<AnyObject>> =
        NSArray::from_retained_slice(&[Retained::into_super(Retained::into_super(a.clone()))]);
    assert_eq!(description(&nested), "(\n    \"{(\\n    1,\\n    2,\\n    3\\n)}\"\n)");
    let dict =
        NSDictionary::from_retained_objects(&[&*s("k")], &[Retained::into_super(Retained::into_super(ordered(&[1])))]);
    assert_eq!(description(&dict), "{\n    k = \"{(\\n    1\\n)}\";\n}");
}

#[test]
fn mutable_ordered_sets() {
    let m = mutable_ordered(&[1, 2, 3]);
    m.addObject(&n(2));
    assert_eq!(members(&m), [1, 2, 3], "adding a member changes nothing");
    m.insertObject_atIndex(&n(0), 0);
    m.insertObject_atIndex(&n(3), 0);
    assert_eq!(members(&m), [0, 1, 2, 3], "inserting a member changes nothing");
    m.removeObjectAtIndex(1);
    assert_eq!(members(&m), [0, 2, 3]);
    m.removeObject(&n(3));
    m.removeObject(&n(42));
    assert_eq!(members(&m), [0, 2]);
    m.replaceObjectAtIndex_withObject(0, &n(5));
    assert_eq!(members(&m), [5, 2]);
    assert_eq!(m.indexOfObject(&n(0)), NSNotFound as NSUInteger);
    assert_eq!(m.indexOfObject(&n(5)), 0);

    // Replacing with a member elsewhere is refused, as in Foundation.
    let m = mutable_ordered(&[1, 2, 3]);
    m.replaceObjectAtIndex_withObject(0, &n(3));
    assert_eq!(members(&m), [1, 2, 3]);
    m.setObject_atIndex(&n(3), 0);
    assert_eq!(members(&m), [1, 2, 3]);
    m.setObject_atIndex(&n(9), 3);
    assert_eq!(members(&m), [1, 2, 3, 9], "setting at the count appends");
    m.setObject_atIndex(&n(1), 4);
    assert_eq!(members(&m), [1, 2, 3, 9]);
    m.setObject_atIndex(&n(7), 0);
    assert_eq!(members(&m), [7, 2, 3, 9]);
    m.exchangeObjectAtIndex_withObjectAtIndex(0, 3);
    assert_eq!(members(&m), [9, 2, 3, 7]);
    assert_eq!(m.indexOfObject(&n(7)), 3);

    let m = mutable_ordered(&[0, 1, 2, 3, 4, 5]);
    m.moveObjectsAtIndexes_toIndex(&indexes(&[1, 3]), 3);
    assert_eq!(members(&m), [0, 2, 4, 1, 3, 5]);
    let m = mutable_ordered(&[0, 1, 2, 3, 4, 5]);
    m.moveObjectsAtIndexes_toIndex(&indexes(&[1, 3]), 0);
    assert_eq!(members(&m), [1, 3, 0, 2, 4, 5]);

    let m = mutable_ordered(&[0, 1, 2]);
    m.insertObjects_atIndexes(&numbers(&[7, 8]), &indexes(&[1, 4]));
    assert_eq!(members(&m), [0, 7, 1, 2, 8]);
    let m = mutable_ordered(&[0, 1, 2, 3]);
    m.replaceObjectsAtIndexes_withObjects(&indexes(&[0, 2]), &numbers(&[3, 9]));
    assert_eq!(members(&m), [1, 3, 9], "the members go, then the new objects come in unless already there");
    let m = mutable_ordered(&[0, 1, 2, 3]);
    let objects = [n(3), n(7), n(8), n(1)];
    let ptrs: Vec<_> = objects.iter().map(|o| NonNull::from(&**o)).collect();
    unsafe { m.replaceObjectsInRange_withObjects_count(NSRange::new(1, 2), ptrs.as_ptr().cast_mut(), 4) };
    assert_eq!(members(&m), [0, 7, 8, 1, 3]);
    m.removeObjectsInRange(NSRange::new(1, 2));
    assert_eq!(members(&m), [0, 1, 3]);
    m.removeObjectsAtIndexes(&indexes(&[0, 2]));
    assert_eq!(members(&m), [1]);
    m.addObjectsFromArray(&numbers(&[1, 4, 5]));
    let objects = [n(6), n(4)];
    let ptrs: Vec<_> = objects.iter().map(|o| NonNull::from(&**o)).collect();
    unsafe { m.addObjects_count(ptrs.as_ptr().cast_mut(), 2) };
    assert_eq!(members(&m), [1, 4, 5, 6]);
    m.removeObjectsInArray(&numbers(&[4, 42, 6]));
    assert_eq!(members(&m), [1, 5]);
    m.removeAllObjects();
    assert_eq!(m.count(), 0);

    // Set algebra keeps the receiver's order and appends in the other's.
    let m = mutable_ordered(&[0, 1, 2, 3]);
    m.intersectOrderedSet(&ordered(&[3, 1, 9]));
    assert_eq!(members(&m), [1, 3]);
    m.unionOrderedSet(&ordered(&[5, 1, 4]));
    assert_eq!(members(&m), [1, 3, 5, 4]);
    m.minusOrderedSet(&ordered(&[5]));
    assert_eq!(members(&m), [1, 3, 4]);
    m.intersectSet(&NSSet::from_retained_slice(&[n(4), n(3)]));
    assert_eq!(members(&m), [3, 4]);
    m.unionSet(&NSSet::from_retained_slice(&[n(7)]));
    assert_eq!(members(&m), [3, 4, 7]);
    m.minusSet(&NSSet::from_retained_slice(&[n(3)]));
    assert_eq!(members(&m), [4, 7]);
    m.unionOrderedSet(&m.copy());
    assert_eq!(members(&m), [4, 7]);

    // Membership tests against both kinds of set.
    let a = ordered(&[1, 2]);
    assert!(a.intersectsOrderedSet(&ordered(&[2, 5])) && !a.intersectsOrderedSet(&ordered(&[5])));
    assert!(a.intersectsSet(&NSSet::from_retained_slice(&[n(1)])));
    assert!(a.isSubsetOfOrderedSet(&ordered(&[0, 1, 2])) && !a.isSubsetOfOrderedSet(&ordered(&[1])));
    assert!(a.isSubsetOfSet(&NSSet::from_retained_slice(&[n(1), n(2), n(3)])));

    // Big sets stay consistent through changes in the middle.
    let m = NSMutableOrderedSet::<NSNumber>::new();
    for i in 0..200 {
        m.addObject(&n(i));
    }
    for i in (0..200).step_by(3) {
        m.removeObject(&n(i));
    }
    m.insertObject_atIndex(&n(1000), 5);
    m.exchangeObjectAtIndex_withObjectAtIndex(0, 100);
    let expected: Vec<i64> = members(&m);
    for (i, v) in expected.iter().enumerate() {
        assert_eq!(m.indexOfObject(&n(*v)), i);
    }
    for i in (0..200).step_by(3) {
        assert!(!m.containsObject(&n(i)));
    }
}

#[test]
fn ordered_sets_fail_like_foundation() {
    let a = ordered(&[1, 2, 3]);
    assert!(failure(|| drop(a.objectAtIndex(5))).ends_with("index 5 beyond bounds [0 .. 2]"));
    let empty = NSOrderedSet::<NSNumber>::new();
    assert!(failure(|| drop(empty.objectAtIndex(0))).ends_with("index 0 beyond bounds for empty ordered set"));
    assert!(
        failure(|| drop(a.objectsAtIndexes(&indexes(&[7])))).ends_with("index 7 in index set beyond bounds [0 .. 2]")
    );
    let reason = failure(|| unsafe {
        let mut buffer = [std::ptr::null_mut::<AnyObject>(); 8];
        let _: () = msg_send![&*a, getObjects: buffer.as_mut_ptr(), range: NSRange::new(1, 5)];
    });
    assert!(reason.ends_with("range {1, 5} extends beyond bounds [0 .. 2]"), "{reason}");
    let reason = failure(|| {
        let _ = unsafe {
            a.indexOfObject_inSortedRange_options_usingComparator(
                &n(1),
                NSRange::new(1, 9),
                NSBinarySearchingOptions(0),
                RcBlock::as_ptr(&compare_numbers()),
            )
        };
    });
    assert!(reason.ends_with("range {1, 9} extends beyond bounds [0 .. 2]"), "{reason}");
    let block = RcBlock::new(|_: NonNull<NSNumber>, _: NSUInteger, _: NonNull<Bool>| {});
    let reason =
        failure(|| a.enumerateObjectsAtIndexes_options_usingBlock(&indexes(&[8]), NSEnumerationOptions(0), &block));
    assert!(reason.ends_with("index 8 beyond bounds [0 .. 2]"), "{reason}");
    let reason = failure(|| unsafe {
        drop(NSOrderedSet::orderedSetWithArray_range_copyItems(&numbers(&[1]), NSRange::new(0, 3), false))
    });
    assert!(reason.ends_with("range {0, 3} extends beyond bounds [0 .. 0]"), "{reason}");
    let reason = failure(|| unsafe {
        drop(NSOrderedSet::orderedSetWithOrderedSet_range_copyItems(&a, NSRange::new(2, 3), false))
    });
    assert!(reason.ends_with("range {2, 3} extends beyond bounds [0 .. 2]"), "{reason}");
    let reason = failure(|| unsafe {
        let objects = [Retained::as_ptr(&n(1)), std::ptr::null()];
        let _: Retained<NSOrderedSet<NSNumber>> =
            msg_send![NSOrderedSet::<NSNumber>::class(), orderedSetWithObjects: objects.as_ptr(), count: 2usize];
    });
    assert!(reason.ends_with("attempt to insert nil object from objects[1]"), "{reason}");

    let m = mutable_ordered(&[1, 2, 3, 4]);
    assert!(failure(|| drop(m.objectAtIndex(9))).ends_with("index 9 beyond bounds [0 .. 3]"));
    assert!(failure(|| m.insertObject_atIndex(&n(9), 9)).ends_with("index 9 beyond bounds [0 .. 3]"));
    assert!(failure(|| m.removeObjectAtIndex(9)).ends_with("index 9 beyond bounds [0 .. 3]"));
    assert!(failure(|| m.replaceObjectAtIndex_withObject(9, &n(9))).ends_with("index 9 beyond bounds [0 .. 3]"));
    assert!(failure(|| m.setObject_atIndex(&n(9), 9)).ends_with("index 9 beyond bounds [0 .. 3]"));
    assert!(failure(|| m.exchangeObjectAtIndex_withObjectAtIndex(0, 9)).ends_with("index 9 beyond bounds [0 .. 3]"));
    assert!(
        failure(|| m.removeObjectsInRange(NSRange::new(2, 9))).ends_with("range {2, 9} extends beyond bounds [0 .. 3]")
    );
    assert!(
        failure(|| m.removeObjectsAtIndexes(&indexes(&[8]))).ends_with("index 8 in index set beyond bounds [0 .. 3]")
    );
    assert!(
        failure(|| m.replaceObjectsAtIndexes_withObjects(&indexes(&[8]), &numbers(&[1])))
            .ends_with("index 8 in index set beyond bounds [0 .. 4]")
    );
    let reason = failure(|| unsafe {
        m.sortRange_options_usingComparator(NSRange::new(1, 9), NSSortOptions(0), RcBlock::as_ptr(&compare_numbers()))
    });
    assert!(reason.ends_with("range {1, 9} extends beyond bounds [0 .. 3]"), "{reason}");
    let m6 = mutable_ordered(&[0, 1, 2, 3, 4, 5]);
    let reason = failure(|| m6.moveObjectsAtIndexes_toIndex(&indexes(&[1, 3]), 5));
    assert!(reason.ends_with("index 5 beyond bounds [0 .. 3]"), "{reason}");
    let m3 = mutable_ordered(&[0, 1, 2]);
    let reason = failure(|| m3.insertObjects_atIndexes(&numbers(&[7]), &indexes(&[1, 2])));
    assert!(reason.ends_with("count of array (1) differs from count of index set (2)"), "{reason}");
    let reason = failure(|| m3.insertObjects_atIndexes(&numbers(&[7]), &indexes(&[1, 4])));
    assert!(reason.ends_with("index 4 in index set beyond bounds [0 .. 3]"), "{reason}");
    let reason = failure(|| m3.replaceObjectsAtIndexes_withObjects(&indexes(&[1, 2]), &numbers(&[7])));
    assert!(reason.ends_with("count of array (1) differs from count of index set (2)"), "{reason}");
    // A member skipped leaves the next run's index past the end.
    let reason = failure(|| m3.insertObjects_atIndexes(&numbers(&[2, 8]), &indexes(&[1, 4])));
    assert!(reason.ends_with("index 4 beyond bounds [0 .. 2]"), "{reason}");
    assert_eq!(members(&m3), [0, 1, 2], "failed operations change nothing");

    for (method, reason) in [
        (
            "addObject",
            failure(|| unsafe {
                let _: () = msg_send![&*m3, addObject: std::ptr::null::<AnyObject>()];
            }),
        ),
        (
            "insert",
            failure(|| unsafe {
                let _: () = msg_send![&*m3, insertObject: std::ptr::null::<AnyObject>(), atIndex: 0usize];
            }),
        ),
        (
            "setObject",
            failure(|| unsafe {
                let _: () = msg_send![&*m3, setObject: std::ptr::null::<AnyObject>(), atIndex: 0usize];
            }),
        ),
        (
            "replace",
            failure(|| unsafe {
                let _: () = msg_send![&*m3, replaceObjectAtIndex: 0usize, withObject: std::ptr::null::<AnyObject>()];
            }),
        ),
    ] {
        assert!(reason.ends_with("object cannot be nil"), "{method}: {reason}");
    }
    let reason = failure(|| unsafe {
        let objects = [Retained::as_ptr(&n(5)), std::ptr::null()];
        let _: () = msg_send![&*m3, addObjects: objects.as_ptr(), count: 2usize];
    });
    assert!(reason.ends_with("attempt to insert nil object from objects[1]"), "{reason}");
    assert_eq!(members(&m3), [0, 1, 2]);

    let empty = NSMutableOrderedSet::<NSNumber>::new();
    assert!(failure(|| empty.removeObjectAtIndex(0)).ends_with("index 0 beyond bounds for empty ordered set"));
    assert!(failure(|| drop(empty.objectAtIndex(0))).ends_with("index 0 beyond bounds for empty ordered set"));
    assert!(failure(|| empty.insertObject_atIndex(&n(1), 1)).ends_with("index 1 beyond bounds for empty ordered set"));
    assert!(
        failure(|| empty.replaceObjectAtIndex_withObject(0, &n(1)))
            .ends_with("index 0 beyond bounds for empty ordered set")
    );
}

#[test]
fn ordered_set_enumeration() {
    let a = ordered(&[5, 6, 7]);
    assert_eq!(fast_numbers(&*a), [5, 6, 7]);
    let m = mutable_ordered(&[1, 2, 3, 4, 5, 6]);
    assert_eq!(fast_numbers(&*m), [1, 2, 3, 4, 5, 6]);
    let e = unsafe { a.objectEnumerator() };
    assert_eq!(values(&e.allObjects()), [5, 6, 7]);
    let e = unsafe { a.reverseObjectEnumerator() };
    let mut seen = vec![];
    while let Some(x) = e.nextObject() {
        seen.push(x.as_i64());
    }
    assert_eq!(seen, [7, 6, 5]);

    // Enumerators walk by position up to the count they started with.
    let m = mutable_ordered(&[0, 1]);
    let e = unsafe { m.objectEnumerator() };
    assert_eq!(e.nextObject().unwrap().as_i64(), 0);
    m.addObject(&n(2));
    assert_eq!(e.nextObject().unwrap().as_i64(), 1);
    assert!(e.nextObject().is_none());
    let m = mutable_ordered(&[0, 1, 2]);
    let e = unsafe { m.reverseObjectEnumerator() };
    assert_eq!(e.nextObject().unwrap().as_i64(), 2);
    m.removeObjectAtIndex(2);
    m.removeObjectAtIndex(1);
    assert!(failure(|| drop(e.nextObject())).ends_with("index 1 beyond bounds [0 .. 0]"));

    // Blocks see every member with its index, in either direction, and
    // may change a mutable set.
    let seen = RefCell::new(Vec::new());
    let block = RcBlock::new(|x: NonNull<NSNumber>, i: NSUInteger, _stop: NonNull<Bool>| {
        seen.borrow_mut().push((unsafe { x.as_ref() }.as_i64(), i));
    });
    a.enumerateObjectsUsingBlock(&block);
    a.enumerateObjectsWithOptions_usingBlock(NSEnumerationOptions::Reverse, &block);
    a.enumerateObjectsAtIndexes_options_usingBlock(&indexes(&[0, 2]), NSEnumerationOptions(0), &block);
    assert_eq!(*seen.borrow(), [(5, 0), (6, 1), (7, 2), (7, 2), (6, 1), (5, 0), (5, 0), (7, 2)]);
    let m = mutable_ordered(&[0, 1]);
    let grow = RcBlock::new(|_: NonNull<NSNumber>, i: NSUInteger, _stop: NonNull<Bool>| {
        if i == 0 {
            m.addObject(&n(5));
        }
    });
    m.enumerateObjectsUsingBlock(&grow);
    assert_eq!(members(&m), [0, 1, 5]);
    let stop_at_one = RcBlock::new(|x: NonNull<NSNumber>, _: NSUInteger, stop: NonNull<Bool>| {
        let v = unsafe { x.as_ref() }.as_i64();
        if v == 6 {
            unsafe { *stop.as_ptr() = Bool::YES };
        }
        Bool::new(v >= 6)
    });
    assert_eq!(a.indexOfObjectPassingTest(&stop_at_one), 1);
    let even = RcBlock::new(|x: NonNull<NSNumber>, _: NSUInteger, _: NonNull<Bool>| {
        Bool::new(unsafe { x.as_ref() }.as_i64() % 2 == 1)
    });
    let odd = a.indexesOfObjectsPassingTest(&even);
    assert_eq!((odd.count(), odd.firstIndex(), odd.lastIndex()), (2, 0, 2));
    assert_eq!(a.indexOfObjectWithOptions_passingTest(NSEnumerationOptions::Reverse, &even), 2);
    let at = a.indexesOfObjectsAtIndexes_options_passingTest(&indexes(&[1, 2]), NSEnumerationOptions(0), &even);
    assert_eq!((at.count(), at.firstIndex()), (1, 2));
    assert_eq!(
        a.indexOfObjectAtIndexes_options_passingTest(&indexes(&[1]), NSEnumerationOptions(0), &even),
        NSNotFound as NSUInteger
    );
    let with_options = a.indexesOfObjectsWithOptions_passingTest(NSEnumerationOptions::Reverse, &even);
    assert_eq!(with_options.count(), 2);
}

#[test]
fn ordered_set_sorting_and_searching() {
    let m = mutable_ordered(&[3, 1, 2, 0]);
    let compare = compare_numbers();
    let sorted = unsafe { m.sortedArrayUsingComparator(RcBlock::as_ptr(&compare)) };
    assert_eq!(values(&sorted), [0, 1, 2, 3]);
    let sorted = unsafe { m.sortedArrayWithOptions_usingComparator(NSSortOptions::Stable, RcBlock::as_ptr(&compare)) };
    assert_eq!(values(&sorted), [0, 1, 2, 3]);
    unsafe { m.sortUsingComparator(RcBlock::as_ptr(&compare)) };
    assert_eq!(members(&m), [0, 1, 2, 3]);
    assert_eq!(m.indexOfObject(&n(2)), 2, "the index follows a sort");
    let m = mutable_ordered(&[5, 4, 3, 2, 1]);
    unsafe { m.sortRange_options_usingComparator(NSRange::new(1, 3), NSSortOptions(0), RcBlock::as_ptr(&compare)) };
    assert_eq!(members(&m), [5, 2, 3, 4, 1]);
    unsafe { m.sortWithOptions_usingComparator(NSSortOptions::Concurrent, RcBlock::as_ptr(&compare)) };
    assert_eq!(members(&m), [1, 2, 3, 4, 5]);
    let found = unsafe {
        m.indexOfObject_inSortedRange_options_usingComparator(
            &n(4),
            NSRange::new(0, 5),
            NSBinarySearchingOptions(0),
            RcBlock::as_ptr(&compare),
        )
    };
    assert_eq!(found, 3);

    let by_value = NSSortDescriptor::sortDescriptorWithKey_ascending(None, false);
    let descriptors = NSArray::from_retained_slice(&[by_value]);
    assert_eq!(values(&ordered(&[3, 1, 2]).sortedArrayUsingDescriptors(&descriptors)), [3, 2, 1]);
    let m = mutable_ordered(&[3, 1, 2, 0]);
    m.sortUsingDescriptors(&descriptors);
    assert_eq!(members(&m), [3, 2, 1, 0]);
    assert_eq!(m.indexOfObject(&n(0)), 3);
}

#[test]
fn ordered_set_views_follow_the_set() {
    let m = mutable_ordered(&[1, 2, 3]);
    let array = m.array();
    let set = m.set();
    let reversed = m.reversedOrderedSet();
    assert!(array.isKindOfClass(NSArray::<AnyObject>::class()));
    assert!(!array.isKindOfClass(NSMutableArray::<AnyObject>::class()));
    assert!(set.isKindOfClass(NSSet::<AnyObject>::class()));
    assert!(!set.isKindOfClass(NSMutableSet::<AnyObject>::class()));
    m.addObject(&n(4));
    assert_eq!(values(&array), [1, 2, 3, 4], "the array view follows changes");
    assert_eq!(array.count(), 4);
    assert_eq!(set.count(), 4, "the set view follows changes");
    assert!(set.containsObject(&n(4)));
    assert_eq!(reversed.count(), 3, "a reversed set doesn't");
    assert_eq!(description(&array), "(\n    1,\n    2,\n    3,\n    4\n)");
    let mut in_set: Vec<i64> = set.allObjects().iter().map(|x| x.as_i64()).collect();
    in_set.sort();
    assert_eq!(in_set, [1, 2, 3, 4]);
    assert!(array.isEqual(Some(&numbers(&[1, 2, 3, 4]))));
    assert!(numbers(&[1, 2, 3, 4]).isEqual(Some(&array)));
    assert!(set.isEqual(Some(&NSSet::from_retained_slice(&[n(1), n(2), n(3), n(4)]))));
    assert_eq!(array.indexOfObject(&n(3)), 2);
    assert!(array.containsObject(&n(1)));
    assert_eq!(fast_numbers(&*array), [1, 2, 3, 4]);
    let mut in_set = fast_numbers(&*set);
    in_set.sort();
    assert_eq!(in_set, [1, 2, 3, 4]);
    // Copies are snapshots.
    let snapshot = array.copy();
    let set_snapshot = set.copy();
    m.addObject(&n(5));
    assert_eq!(values(&snapshot), [1, 2, 3, 4]);
    assert_eq!(set_snapshot.count(), 4);
    assert_eq!(array.count(), 5);
    let mutable_copy = array.mutableCopy();
    mutable_copy.addObject(&n(6));
    assert_eq!(values(&mutable_copy), [1, 2, 3, 4, 5, 6]);
    assert!(failure(|| drop(array.objectAtIndex(9))).ends_with("index 9 beyond bounds [0 .. 4]"));
    // NSArray's other methods work on the view.
    assert_eq!(values(&array.subarrayWithRange(NSRange::new(1, 2))), [2, 3]);
    assert_eq!(array.lastObject().unwrap().as_i64(), 5);
}

// NSSortDescriptor

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "Collections2Person"]
    #[ivars = (String, i32, f64, bool)]
    struct Person;

    impl Person {
        #[unsafe(method_id(name))]
        fn name(&self) -> Retained<NSString> {
            s(&self.ivars().0)
        }

        #[unsafe(method(age))]
        fn age(&self) -> i32 {
            self.ivars().1
        }

        #[unsafe(method(height))]
        fn height(&self) -> f64 {
            self.ivars().2
        }

        #[unsafe(method(isRetired))]
        fn is_retired(&self) -> bool {
            self.ivars().3
        }

        #[unsafe(method_id(boss))]
        fn boss(&self) -> Option<Retained<AnyObject>> {
            None
        }
    }
);

impl Person {
    fn new(name: &str, age: i32, height: f64, retired: bool) -> Retained<Person> {
        let this = Person::alloc().set_ivars((name.to_owned(), age, height, retired));
        unsafe { msg_send![super(this), init] }
    }
}

fn record(a: i64, b: i64) -> Retained<NSDictionary<NSString, NSNumber>> {
    NSDictionary::from_retained_objects(&[&*s("a"), &*s("b")], &[n(a), n(b)])
}

fn ab(array: &NSArray<NSDictionary<NSString, NSNumber>>) -> Vec<(i64, i64)> {
    array
        .iter()
        .map(|d| (d.objectForKey(&s("a")).unwrap().as_i64(), d.objectForKey(&s("b")).unwrap().as_i64()))
        .collect()
}

#[test]
fn sort_descriptors_describe_themselves() {
    let a = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&s("age")), true);
    assert_eq!(description(&a), "(age, ascending, NO, compare:)");
    assert_eq!(a.key().unwrap().to_string(), "age");
    assert!(a.ascending());
    assert_eq!(a.selector(), Some(sel!(compare:)));
    assert!(unsafe { a.comparator() }.is_null());
    let b = unsafe {
        NSSortDescriptor::sortDescriptorWithKey_ascending_selector(
            Some(&s("name")),
            false,
            Some(sel!(caseInsensitiveCompare:)),
        )
    };
    assert_eq!(description(&b), "(name, descending, NO, caseInsensitiveCompare:)");
    let compare = compare_numbers();
    let c = unsafe {
        NSSortDescriptor::sortDescriptorWithKey_ascending_comparator(Some(&s("x")), true, RcBlock::as_ptr(&compare))
    };
    assert!(description(&c).starts_with("(x, ascending, NO, BLOCK(0x"), "{}", description(&c));
    assert_eq!(c.selector(), None);
    assert!(!unsafe { c.comparator() }.is_null());
    let no_key = NSSortDescriptor::sortDescriptorWithKey_ascending(None, true);
    assert_eq!(description(&no_key), "(, ascending, NO, compare:)");
    assert!(no_key.key().is_none());
    let plain = NSSortDescriptor::new();
    assert_eq!(description(&plain), "(, descending, NO, (null))");
    assert!(plain.key().is_none() && !plain.ascending() && plain.selector().is_none());
    let init = NSSortDescriptor::initWithKey_ascending(NSSortDescriptor::alloc(), Some(&s("k")), false);
    assert_eq!(description(&init), "(k, descending, NO, compare:)");

    let reversed: Retained<NSSortDescriptor> = unsafe { Retained::cast_unchecked(a.reversedSortDescriptor()) };
    assert_eq!(description(&reversed), "(age, descending, NO, compare:)");
    assert!(a.isEqual(Some(&NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&s("age")), true))));
    assert!(!a.isEqual(Some(&reversed)));
    assert!(!a.isEqual(Some(&b)));
    assert!(std::ptr::eq(&*a.copy(), &*a));
    a.allowEvaluation();
}

#[test]
fn sort_descriptors_compare_by_key_paths() {
    let by_age = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&s("age")), true);
    let reversed: Retained<NSSortDescriptor> = unsafe { Retained::cast_unchecked(by_age.reversedSortDescriptor()) };
    let three = NSDictionary::from_retained_objects(&[&*s("age")], &[n(3)]);
    let five = NSDictionary::from_retained_objects(&[&*s("age")], &[n(5)]);
    let missing = NSDictionary::<NSString, NSNumber>::new();
    use objc2_foundation::NSComparisonResult::{Ascending, Descending, Same};
    assert_eq!(unsafe { by_age.compareObject_toObject(&three, &five) }, Ascending);
    assert_eq!(unsafe { reversed.compareObject_toObject(&three, &five) }, Descending);
    assert_eq!(unsafe { by_age.compareObject_toObject(&five, &five) }, Same);
    // Nil values sort first.
    assert_eq!(unsafe { by_age.compareObject_toObject(&missing, &five) }, Ascending);
    assert_eq!(unsafe { by_age.compareObject_toObject(&five, &missing) }, Descending);
    assert_eq!(unsafe { by_age.compareObject_toObject(&missing, &missing) }, Same);
    // Key paths go through each step.
    let outer = NSDictionary::from_retained_objects(&[&*s("person")], std::slice::from_ref(&three));
    let path = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&s("person.age")), true);
    assert_eq!(
        unsafe {
            path.compareObject_toObject(
                &outer,
                &NSDictionary::from_retained_objects(&[&*s("person")], std::slice::from_ref(&five)),
            )
        },
        Ascending
    );
    // Dictionaries answer -valueForKey:.
    assert_eq!(three.valueForKey(&s("age")).map(|v| description(&v)), Some("3".to_owned()));
    assert!(three.valueForKey(&s("nope")).is_none());
    assert_eq!(three.valueForKey(&s("@count")).map(|v| description(&v)), Some("1".to_owned()));
    // An object without the key fails as key-value coding does.
    let object = NSObject::new();
    let reason = failure(|| {
        let _ = unsafe { by_age.compareObject_toObject(&object, &object) };
    });
    assert!(
        reason.ends_with("valueForUndefinedKey:]: this class is not key value coding-compliant for the key age."),
        "{reason}"
    );
    assert!(reason.starts_with("[<NSObject 0x"), "{reason}");
}

#[test]
fn sorting_by_descriptors() {
    let array = numbers(&[3, 1, 2]);
    let ascending = NSSortDescriptor::sortDescriptorWithKey_ascending(None, true);
    let by_self = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&s("self")), false);
    assert_eq!(
        values(&array.sortedArrayUsingDescriptors(&NSArray::from_retained_slice(std::slice::from_ref(&ascending)))),
        [1, 2, 3]
    );
    assert_eq!(values(&array.sortedArrayUsingDescriptors(&NSArray::from_retained_slice(&[by_self]))), [3, 2, 1]);
    assert_eq!(values(&array.sortedArrayUsingDescriptors(&NSArray::new())), [3, 1, 2], "no descriptors, no change");
    let set = NSSet::from_retained_slice(&[n(3), n(1), n(2)]);
    assert_eq!(
        values(&set.sortedArrayUsingDescriptors(&NSArray::from_retained_slice(std::slice::from_ref(&ascending)))),
        [1, 2, 3]
    );

    // Several keys: ties on the first fall to the second; equal records
    // keep their order.
    let records =
        NSMutableArray::from_retained_slice(&[record(1, 2), record(0, 5), record(1, 1), record(0, 3), record(0, 5)]);
    let by_a = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&s("a")), true);
    let by_b = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&s("b")), false);
    let first = records.objectAtIndex(1);
    records.sortUsingDescriptors(&NSArray::from_retained_slice(&[by_a.clone(), by_b]));
    assert_eq!(ab(&records), [(0, 5), (0, 5), (0, 3), (1, 2), (1, 1)]);
    assert!(std::ptr::eq(&*records.objectAtIndex(0), &*first), "the sort is stable");

    // Getters: objects, numbers, floating point and BOOL, wrapped in
    // NSNumber as key-value coding wraps them.
    let people = NSArray::from_retained_slice(&[
        Person::new("Cy", 40, 1.7, true),
        Person::new("Al", 30, 1.9, false),
        Person::new("Bo", 50, 1.8, true),
    ]);
    let names = |array: &NSArray<Person>| array.iter().map(|p| p.ivars().0.clone()).collect::<Vec<_>>();
    let by = |key: &str, ascending: bool| {
        NSArray::from_retained_slice(&[NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&s(key)), ascending)])
    };
    assert_eq!(names(&people.sortedArrayUsingDescriptors(&by("age", true))), ["Al", "Cy", "Bo"]);
    assert_eq!(names(&people.sortedArrayUsingDescriptors(&by("height", false))), ["Al", "Bo", "Cy"]);
    assert_eq!(names(&people.sortedArrayUsingDescriptors(&by("retired", true))), ["Al", "Cy", "Bo"]);
    let by_retired_then_age = NSArray::from_retained_slice(&[
        NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&s("retired")), false),
        NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&s("age")), false),
    ]);
    assert_eq!(names(&people.sortedArrayUsingDescriptors(&by_retired_then_age)), ["Bo", "Cy", "Al"]);
    // A nil object along the path is a nil value, which sorts first.
    assert_eq!(names(&people.sortedArrayUsingDescriptors(&by("boss.age", true))), ["Cy", "Al", "Bo"]);

    // A comparator block, reversed by the descriptor's direction.
    let compare = compare_numbers();
    let blocky =
        unsafe { NSSortDescriptor::sortDescriptorWithKey_ascending_comparator(None, false, RcBlock::as_ptr(&compare)) };
    assert_eq!(
        values(&numbers(&[2, 3, 1]).sortedArrayUsingDescriptors(&NSArray::from_retained_slice(&[blocky]))),
        [3, 2, 1]
    );
    // A selector the values answer.
    let by_selector =
        unsafe { NSSortDescriptor::sortDescriptorWithKey_ascending_selector(None, true, Some(sel!(compare:))) };
    let sorted = numbers(&[2, 3, 1]).sortedArrayUsingDescriptors(&NSArray::from_retained_slice(&[by_selector]));
    assert_eq!(values(&sorted), [1, 2, 3]);
    let bad =
        unsafe { NSSortDescriptor::sortDescriptorWithKey_ascending_selector(None, true, Some(sel!(noSuchCompare:))) };
    let reason = failure(|| drop(numbers(&[1, 2]).sortedArrayUsingDescriptors(&NSArray::from_retained_slice(&[bad]))));
    assert!(reason.contains("noSuchCompare:]: unrecognized selector sent to instance"), "{reason}");

    let m = NSMutableArray::from_retained_slice(&[n(3), n(1), n(2)]);
    m.sortUsingDescriptors(&NSArray::from_retained_slice(&[ascending]));
    assert_eq!(values(&m), [1, 2, 3]);
    // Large arrays of numbers sort without trouble.
    let big: Vec<i64> = (0..2000).map(|i| (i * 7919) % 2003).collect();
    let mut expected = big.clone();
    expected.sort();
    let by_value = NSArray::from_retained_slice(&[NSSortDescriptor::sortDescriptorWithKey_ascending(None, true)]);
    assert_eq!(values(&numbers(&big).sortedArrayUsingDescriptors(&by_value)), expected);
}

struct ChainIvars {
    items: RefCell<Vec<Retained<NSNumber>>>,
}

define_class!(
    /// A mutable ordered set defined outside the framework, keeping its own
    /// members behind NSMutableOrderedSet's primitive methods.
    #[unsafe(super(NSMutableOrderedSet))]
    #[name = "Collections2Chain"]
    #[ivars = ChainIvars]
    struct Chain;

    impl Chain {
        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.ivars().items.borrow().len()
        }

        #[unsafe(method(objectAtIndex:))]
        fn object_at_index(&self, index: NSUInteger) -> *mut NSNumber {
            Retained::as_ptr(&self.ivars().items.borrow()[index]).cast_mut()
        }

        #[unsafe(method(indexOfObject:))]
        fn index_of_object(&self, object: &NSNumber) -> NSUInteger {
            let items = self.ivars().items.borrow();
            items.iter().position(|x| x.isEqualToNumber(object)).unwrap_or(NSNotFound as NSUInteger)
        }

        #[unsafe(method(insertObject:atIndex:))]
        fn insert_object(&self, object: &NSNumber, index: NSUInteger) {
            let mut items = self.ivars().items.borrow_mut();
            if !items.iter().any(|x| x.isEqualToNumber(object)) {
                items.insert(index, object.retain());
            }
        }

        #[unsafe(method(removeObjectAtIndex:))]
        fn remove_object_at_index(&self, index: NSUInteger) {
            let removed = self.ivars().items.borrow_mut().remove(index);
            drop(removed);
        }

        #[unsafe(method(replaceObjectAtIndex:withObject:))]
        fn replace_object_at_index(&self, index: NSUInteger, object: &NSNumber) {
            let old = std::mem::replace(&mut self.ivars().items.borrow_mut()[index], object.retain());
            drop(old);
        }
    }
);

impl Chain {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(ChainIvars { items: RefCell::new(Vec::new()) });
        unsafe { msg_send![super(this), init] }
    }

    fn own(&self) -> Vec<i64> {
        self.ivars().items.borrow().iter().map(|x| x.as_i64()).collect()
    }
}

#[test]
fn ordered_set_subclasses_work_through_their_primitives() {
    let chain = Chain::new();
    let m: &NSMutableOrderedSet<NSNumber> =
        unsafe { &*(Retained::as_ptr(&chain) as *const NSMutableOrderedSet<NSNumber>) };
    m.addObject(&n(1));
    m.addObjectsFromArray(&numbers(&[2, 3, 2]));
    assert_eq!(chain.own(), [1, 2, 3]);
    assert!(m.containsObject(&n(2)) && !m.containsObject(&n(7)));
    assert_eq!(m.indexOfObject(&n(3)), 2);
    assert_eq!(m.firstObject().unwrap().as_i64(), 1);
    assert_eq!(m.lastObject().unwrap().as_i64(), 3);
    m.removeObject(&n(2));
    assert_eq!(chain.own(), [1, 3]);
    m.unionSet(&NSSet::from_retained_slice(&[n(5)]));
    m.unionOrderedSet(&ordered(&[1, 6]));
    assert_eq!(chain.own(), [1, 3, 5, 6]);
    m.exchangeObjectAtIndex_withObjectAtIndex(0, 3);
    assert_eq!(chain.own(), [6, 3, 5, 1]);
    unsafe { m.sortUsingComparator(RcBlock::as_ptr(&compare_numbers())) };
    assert_eq!(chain.own(), [1, 3, 5, 6]);
    m.intersectOrderedSet(&ordered(&[6, 5, 3]));
    assert_eq!(chain.own(), [3, 5, 6]);
    m.moveObjectsAtIndexes_toIndex(&indexes(&[0]), 2);
    assert_eq!(chain.own(), [5, 6, 3]);
    m.insertObjects_atIndexes(&numbers(&[8, 9]), &indexes(&[0, 1]));
    assert_eq!(chain.own(), [8, 9, 5, 6, 3]);
    m.removeObjectsInRange(NSRange::new(1, 2));
    assert_eq!(chain.own(), [8, 6, 3]);
    m.setObject_atIndex(&n(4), 0);
    assert_eq!(chain.own(), [4, 6, 3]);
    assert_eq!(description(m), "{(\n    4,\n    6,\n    3\n)}");
    assert_eq!(values(&m.array()), [4, 6, 3]);
    assert_eq!(fast_numbers(m), [4, 6, 3]);
    assert!(m.isEqual(Some(&ordered(&[4, 6, 3]))));
    assert_eq!(members(&m.copy()), [4, 6, 3]);
    m.removeAllObjects();
    assert_eq!((chain.own(), m.count()), (vec![], 0));
}

// NSHashTable and NSMapTable

define_class!(
    /// An object compared by value, whose copies are new objects.
    #[unsafe(super(NSObject))]
    #[name = "Collections2Token"]
    #[ivars = i64]
    struct Token;

    impl Token {
        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<Token>()).is_some_and(|o| *o.ivars() == *self.ivars())
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            *self.ivars() as NSUInteger
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut objc2_foundation::NSZone) -> Retained<Token> {
            Token::new(*self.ivars())
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            s(&format!("token {}", self.ivars()))
        }
    }
);

impl Token {
    fn new(value: i64) -> Retained<Token> {
        let this = Token::alloc().set_ivars(value);
        unsafe { msg_send![super(this), init] }
    }
}

fn tokens(set: &[Retained<AnyObject>]) -> Vec<i64> {
    let mut v: Vec<i64> = set.iter().map(|o| *o.downcast_ref::<Token>().unwrap().ivars()).collect();
    v.sort();
    v
}

fn sorted_numbers(array: &NSArray<NSNumber>) -> Vec<i64> {
    let mut v = values(array);
    v.sort();
    v
}

#[test]
#[allow(deprecated)] // `usesWeakReadAndWriteBarriers`, and the old spelling of `weakObjectsHashTable`
fn weak_hash_tables_forget_objects_that_go() {
    let table = NSHashTable::<Token>::weakObjectsHashTable();
    assert!(table.pointerFunctions().usesWeakReadAndWriteBarriers());
    let a = Token::new(1);
    let b = Token::new(2);
    // Foundation's weak loads autorelease what they load: the pools keep
    // the members from lingering.
    autoreleasepool(|_| {
        table.addObject(Some(&a));
        table.addObject(Some(&b));
        table.addObject(Some(&Token::new(1)));
        assert_eq!(table.count(), 2);
        assert!(table.containsObject(Some(&Token::new(2))));
        assert!(std::ptr::eq(&*table.member(Some(&Token::new(1))).unwrap(), &*a), "the member, not the probe");
    });
    // Weak: the table doesn't keep its members alive.
    autoreleasepool(|_| {
        let c = Token::new(3);
        table.addObject(Some(&c));
        assert!(table.containsObject(Some(&c)));
    });
    autoreleasepool(|_| assert!(!table.containsObject(Some(&Token::new(3)))));
    // Whatever Foundation autoreleases while reading goes with the pool.
    let live = |table: &NSHashTable<Token>| {
        autoreleasepool(|_| {
            tokens(
                &table
                    .allObjects()
                    .to_vec()
                    .into_iter()
                    .map(|t| Retained::into_super(Retained::into_super(t)))
                    .collect::<Vec<_>>(),
            )
        })
    };
    assert_eq!(live(&table), [1, 2]);
    drop(b);
    assert_eq!(live(&table), [1]);
    assert!(table.member(Some(&Token::new(2))).is_none());
    assert!(std::ptr::eq(&*table.anyObject().unwrap(), &*a));
    let enumerated = fast_enumerate(&*table);
    assert_eq!(enumerated.len(), 1);
    assert!(std::ptr::eq(enumerated[0].cast::<Token>(), &*a));
    assert_eq!(table.setRepresentation().count(), 1);
    // Foundation's count may include members that went until the table
    // next tidies up; Sidestep's is exact.
    assert!(table.count() >= 1);
    #[cfg(not(target_vendor = "apple"))]
    assert_eq!(table.count(), 1);

    // Many members going and coming leave the table working.
    for i in 0..500 {
        autoreleasepool(|_| table.addObject(Some(&Token::new(100 + i))));
    }
    let keep: Vec<_> = (0..50).map(|i| Token::new(1000 + i)).collect();
    for t in &keep {
        table.addObject(Some(t));
    }
    for t in &keep {
        assert!(table.containsObject(Some(t)));
    }
    assert_eq!(table.allObjects().count(), 51);
    #[cfg(not(target_vendor = "apple"))]
    assert_eq!(table.count(), 51);
    table.removeObject(Some(&Token::new(1)));
    assert_eq!(table.allObjects().count(), 50);
    table.removeAllObjects();
    assert_eq!(table.count(), 0);
    let old = NSHashTable::<Token>::hashTableWithWeakObjects();
    assert!(is_kind(&old, NSHashTable::<AnyObject>::class()));
}

#[test]
#[allow(deprecated)] // `usesWeakReadAndWriteBarriers`
fn hash_tables_by_options() {
    use objc2_foundation::NSPointerFunctionsOptions as Options;
    let strong = NSHashTable::<NSNumber>::hashTableWithOptions(Options::StrongMemory);
    strong.addObject(Some(&n(1)));
    strong.addObject(Some(&NSNumber::new_f64(1.0)));
    assert_eq!(strong.count(), 1, "equal objects are one member");
    assert!(strong.containsObject(Some(&n(1))));
    assert!(!strong.pointerFunctions().usesWeakReadAndWriteBarriers());
    // Nil is ignored.
    strong.addObject(None);
    strong.removeObject(None);
    assert!(strong.member(None).is_none() && !strong.containsObject(None));
    assert_eq!(strong.count(), 1);

    // Strong members stay alive.
    let kept = NSHashTable::<Token>::hashTableWithOptions(Options::StrongMemory);
    autoreleasepool(|_| kept.addObject(Some(&Token::new(7))));
    assert!(kept.containsObject(Some(&Token::new(7))));

    // By address: equal objects are different members.
    let by_address = NSHashTable::<Token>::hashTableWithOptions(Options::ObjectPointerPersonality);
    let (x, y) = (Token::new(5), Token::new(5));
    by_address.addObject(Some(&x));
    by_address.addObject(Some(&y));
    by_address.addObject(Some(&x));
    assert_eq!(by_address.count(), 2);
    assert!(by_address.member(Some(&Token::new(5))).is_none());
    assert!(std::ptr::eq(&*by_address.member(Some(&y)).unwrap(), &*y));
    by_address.removeObject(Some(&x));
    assert_eq!(by_address.count(), 1);
    assert!(by_address.containsObject(Some(&y)) && !by_address.containsObject(Some(&x)));

    // Copied in: the member is a copy.
    let copying = NSHashTable::<Token>::hashTableWithOptions(Options::CopyIn);
    let original = Token::new(9);
    copying.addObject(Some(&original));
    let member = copying.anyObject().unwrap();
    assert!(!std::ptr::eq(&*member, &*original));
    assert_eq!(*member.ivars(), 9);

    // Set algebra.
    let h1 = NSHashTable::<NSNumber>::hashTableWithOptions(Options::StrongMemory);
    let h2 = NSHashTable::<NSNumber>::hashTableWithOptions(Options::StrongMemory);
    for i in 0..4 {
        h1.addObject(Some(&n(i)));
    }
    for i in 2..6 {
        h2.addObject(Some(&n(i)));
    }
    assert!(h1.intersectsHashTable(&h2) && !h1.isSubsetOfHashTable(&h2) && !h1.isEqualToHashTable(&h2));
    let copy = h1.copy();
    assert!(!std::ptr::eq(&*copy, &*h1));
    assert!(copy.isEqualToHashTable(&h1) && copy.isEqual(Some(&h1)));
    assert_eq!(copy.hash(), 4);
    h1.intersectHashTable(&h2);
    assert_eq!(sorted_numbers(&h1.allObjects()), [2, 3]);
    assert!(h1.isSubsetOfHashTable(&h2));
    h1.unionHashTable(&h2);
    assert_eq!(sorted_numbers(&h1.allObjects()), [2, 3, 4, 5]);
    assert!(h1.isEqualToHashTable(&h2));
    h1.minusHashTable(&h2);
    assert_eq!(h1.count(), 0);
    assert_eq!(copy.count(), 4, "copies don't follow changes");
    let set = copy.setRepresentation();
    assert!(set.isKindOfClass(NSSet::<AnyObject>::class()));
    assert!(set.isEqualToSet(&NSSet::from_retained_slice(&[n(0), n(1), n(2), n(3)])));
    let mut seen = fast_numbers(&*copy);
    seen.sort();
    assert_eq!(seen, [0, 1, 2, 3]);

    // Descriptions list the members, one per line.
    let one = NSHashTable::<NSNumber>::hashTableWithOptions(Options::StrongMemory);
    assert_eq!(description(&one), "NSHashTable {\n}\n");
    one.addObject(Some(&n(42)));
    let text = description(&one);
    assert!(text.starts_with("NSHashTable {\n[") && text.ends_with("] 42\n}\n"), "{text:?}");

    // Enumerators fail if the table changes before they're done.
    let e = unsafe { copy.objectEnumerator() };
    let _ = e.nextObject();
    copy.addObject(Some(&n(10)));
    let reason = failure(|| while e.nextObject().is_some() {});
    assert!(reason.ends_with("was mutated while being enumerated."), "{reason}");
    let e = unsafe { copy.objectEnumerator() };
    assert_eq!(e.allObjects().count(), 5);
}

#[test]
#[allow(deprecated)] // `usesWeakReadAndWriteBarriers`, and the old spellings of the factories
fn map_tables() {
    // Weak values go with their objects.
    let to_weak = NSMapTable::<NSString, Token>::strongToWeakObjectsMapTable();
    let kept = Token::new(1);
    to_weak.setObject_forKey(Some(&kept), Some(&s("k")));
    autoreleasepool(|_| to_weak.setObject_forKey(Some(&Token::new(2)), Some(&s("gone"))));
    assert!(to_weak.objectForKey(Some(&s("gone"))).is_none());
    assert!(std::ptr::eq(&*to_weak.objectForKey(Some(&s("k"))).unwrap(), &*kept));
    assert_eq!(to_weak.dictionaryRepresentation().count(), 1);
    let keys = unsafe { to_weak.keyEnumerator() }.allObjects();
    assert_eq!(keys.iter().map(|k| k.to_string()).collect::<Vec<_>>(), ["k"]);
    #[cfg(not(target_vendor = "apple"))]
    assert_eq!(to_weak.count(), 1);

    // Weak keys too.
    let from_weak = NSMapTable::<Token, NSNumber>::weakToStrongObjectsMapTable();
    let key = Token::new(5);
    from_weak.setObject_forKey(Some(&n(50)), Some(&key));
    autoreleasepool(|_| from_weak.setObject_forKey(Some(&n(60)), Some(&Token::new(6))));
    assert_eq!(from_weak.objectForKey(Some(&Token::new(5))).unwrap().as_i64(), 50, "keys compare by -isEqual:");
    assert!(from_weak.objectForKey(Some(&Token::new(6))).is_none());
    let live_keys = unsafe { from_weak.keyEnumerator() }.allObjects();
    assert_eq!(live_keys.count(), 1);
    assert!(std::ptr::eq(&*live_keys.objectAtIndex(0), &*key));
    assert_eq!(values(&unsafe { from_weak.objectEnumerator() }.unwrap().allObjects()), [50]);
    #[cfg(not(target_vendor = "apple"))]
    assert_eq!(from_weak.count(), 1);
    // Nil keys and values are ignored.
    from_weak.setObject_forKey(None, Some(&key));
    from_weak.setObject_forKey(Some(&n(1)), None);
    from_weak.removeObjectForKey(None);
    assert!(from_weak.objectForKey(None).is_none());
    assert_eq!(from_weak.objectForKey(Some(&key)).unwrap().as_i64(), 50);
    // Replacing and removing.
    from_weak.setObject_forKey(Some(&n(51)), Some(&Token::new(5)));
    assert_eq!(from_weak.objectForKey(Some(&key)).unwrap().as_i64(), 51);
    from_weak.removeObjectForKey(Some(&key));
    assert!(from_weak.objectForKey(Some(&key)).is_none());
    #[cfg(not(target_vendor = "apple"))]
    assert_eq!(from_weak.count(), 0);

    // Strong to strong, by default and by the old spelling.
    let strong = NSMapTable::<NSString, NSNumber>::new();
    strong.setObject_forKey(Some(&n(1)), Some(&s("one")));
    strong.setObject_forKey(Some(&n(2)), Some(&s("two")));
    assert_eq!(strong.count(), 2);
    assert_eq!(strong.objectForKey(Some(&s("two"))).unwrap().as_i64(), 2);
    let dict = strong.dictionaryRepresentation();
    assert!(dict.isKindOfClass(NSDictionary::<AnyObject, AnyObject>::class()));
    assert_eq!(dict.objectForKey(&s("one")).unwrap().as_i64(), 1);
    let copy = strong.copy();
    assert!(copy.isEqual(Some(&strong)) && !std::ptr::eq(&*copy, &*strong));
    strong.setObject_forKey(Some(&n(3)), Some(&s("three")));
    assert_eq!(copy.count(), 2, "copies don't follow changes");
    let mut keys: Vec<String> =
        fast_enumerate(&*strong).into_iter().map(|p| unsafe { &*p.cast::<NSString>() }.to_string()).collect();
    keys.sort();
    assert_eq!(keys, ["one", "three", "two"], "fast enumeration hands out the keys");
    let e = unsafe { strong.keyEnumerator() };
    let _ = e.nextObject();
    strong.setObject_forKey(Some(&n(4)), Some(&s("four")));
    let reason = failure(|| while e.nextObject().is_some() {});
    assert!(reason.ends_with("was mutated while being enumerated."), "{reason}");
    strong.removeAllObjects();
    assert_eq!(strong.count(), 0);
    let old: Retained<NSMapTable<NSString, NSNumber>> =
        unsafe { Retained::cast_unchecked(NSMapTable::<NSString, NSNumber>::mapTableWithStrongToStrongObjects()) };
    old.setObject_forKey(Some(&n(1)), Some(&s("a")));
    assert_eq!(old.count(), 1);
    for table in [
        NSMapTable::<AnyObject, AnyObject>::mapTableWithWeakToStrongObjects(),
        NSMapTable::<AnyObject, AnyObject>::mapTableWithStrongToWeakObjects(),
        NSMapTable::<AnyObject, AnyObject>::mapTableWithWeakToWeakObjects(),
    ] {
        assert!(is_kind(&table, NSMapTable::<AnyObject, AnyObject>::class()));
    }
    let both_weak = NSMapTable::<Token, Token>::weakToWeakObjectsMapTable();
    both_weak.setObject_forKey(Some(&kept), Some(&key));
    assert!(std::ptr::eq(&*both_weak.objectForKey(Some(&key)).unwrap(), &*kept));
    assert!(both_weak.keyPointerFunctions().usesWeakReadAndWriteBarriers());

    // Keys copied in, or compared by address.
    use objc2_foundation::NSPointerFunctionsOptions as Options;
    let copying =
        NSMapTable::<Token, NSNumber>::mapTableWithKeyOptions_valueOptions(Options::CopyIn, Options::StrongMemory);
    let original = Token::new(3);
    copying.setObject_forKey(Some(&n(1)), Some(&original));
    let stored = unsafe { copying.keyEnumerator() }.nextObject().unwrap();
    assert!(!std::ptr::eq(&*stored, &*original));
    assert_eq!(copying.objectForKey(Some(&original)).unwrap().as_i64(), 1);
    let by_address = NSMapTable::<Token, NSNumber>::mapTableWithKeyOptions_valueOptions(
        Options::ObjectPointerPersonality,
        Options::StrongMemory,
    );
    by_address.setObject_forKey(Some(&n(1)), Some(&original));
    assert!(by_address.objectForKey(Some(&Token::new(3))).is_none());
    assert_eq!(by_address.objectForKey(Some(&original)).unwrap().as_i64(), 1);
    assert!(!by_address.keyPointerFunctions().usesWeakReadAndWriteBarriers());

    // Descriptions show keys and values.
    let one = NSMapTable::<NSString, NSNumber>::strongToStrongObjectsMapTable();
    assert_eq!(description(&one), "NSMapTable {\n}\n");
    one.setObject_forKey(Some(&n(7)), Some(&s("seven")));
    let text = description(&one);
    assert!(text.starts_with("NSMapTable {\n[") && text.ends_with("] seven -> 7\n}\n"), "{text:?}");

    // Big tables keep working through changes.
    let big = NSMapTable::<NSNumber, NSNumber>::strongToStrongObjectsMapTable();
    for i in 0..300 {
        big.setObject_forKey(Some(&n(i * 10)), Some(&n(i)));
    }
    for i in (0..300).step_by(2) {
        big.removeObjectForKey(Some(&n(i)));
    }
    assert_eq!(big.count(), 150);
    for i in 0..300 {
        assert_eq!(big.objectForKey(Some(&n(i))).map(|v| v.as_i64()), (i % 2 == 1).then_some(i * 10));
    }
}

// NSPointerArray

fn pointer_of(obj: &AnyObject) -> *mut std::ffi::c_void {
    (obj as *const AnyObject).cast_mut().cast()
}

#[test]
#[allow(deprecated)] // the old spellings of the factories
fn pointer_arrays() {
    let weak = NSPointerArray::weakObjectsPointerArray();
    let a = Token::new(1);
    autoreleasepool(|_| {
        unsafe { weak.addPointer(pointer_of(&a)) };
        unsafe { weak.addPointer(std::ptr::null_mut()) };
        let b = Token::new(2);
        unsafe { weak.addPointer(pointer_of(&b)) };
        assert_eq!(weak.count(), 3);
        assert!(!weak.pointerAtIndex(2).is_null());
    });
    // The weak reference to the object that went reads null, in its place.
    assert_eq!(weak.count(), 3);
    assert_eq!(weak.pointerAtIndex(0), pointer_of(&a));
    assert!(weak.pointerAtIndex(1).is_null() && weak.pointerAtIndex(2).is_null());
    assert_eq!(weak.allObjects().count(), 1);
    weak.compact();
    assert_eq!(weak.count(), 1);
    assert_eq!(weak.pointerAtIndex(0), pointer_of(&a));
    assert!(
        failure(|| {
            let _ = weak.pointerAtIndex(5);
        })
        .ends_with("attempt to access pointer at index 5 beyond bounds 1")
    );
    assert!(failure(|| weak.removePointerAtIndex(5)).ends_with("attempt to remove pointer at index 5 beyond bounds 1"));
    assert!(
        failure(|| unsafe { weak.insertPointer_atIndex(std::ptr::null_mut(), 5) })
            .ends_with("attempt to insert pointer at index 5 beyond bounds 1")
    );
    assert!(
        failure(|| unsafe { weak.replacePointerAtIndex_withPointer(5, std::ptr::null_mut()) })
            .ends_with("attempt to replace pointer at index 5 beyond bounds 1")
    );
    weak.setCount(4);
    assert_eq!(weak.count(), 4);
    assert!(weak.pointerAtIndex(3).is_null());
    weak.setCount(1);
    assert_eq!(weak.count(), 1);

    let strong = NSPointerArray::strongObjectsPointerArray();
    let obj = Token::new(3);
    let before = obj.retainCount();
    // Foundation may autorelease what it hands out: the pool lets it go
    // before counting.
    autoreleasepool(|_| {
        unsafe { strong.addPointer(pointer_of(&obj)) };
        assert!(obj.retainCount() > before, "strong pointer arrays retain");
        unsafe { strong.addPointer(std::ptr::null_mut()) };
        unsafe { strong.insertPointer_atIndex(pointer_of(&a), 0) };
        assert_eq!(strong.count(), 3);
        assert_eq!(strong.pointerAtIndex(0), pointer_of(&a));
        assert_eq!(strong.pointerAtIndex(1), pointer_of(&obj));
        assert_eq!(strong.allObjects().count(), 2);
        let enumerated = fast_enumerate(&*strong);
        assert_eq!(enumerated.len(), 3, "fast enumeration hands out the nulls too");
        assert!(enumerated[2].is_null());
        unsafe { strong.replacePointerAtIndex_withPointer(2, pointer_of(&obj)) };
        strong.removePointerAtIndex(1);
        assert_eq!(strong.count(), 2);
        let copy = strong.copy();
        assert_eq!(copy.count(), 2);
        assert_eq!(copy.pointerAtIndex(1), pointer_of(&obj));
        strong.setCount(0);
        assert_eq!((strong.count(), copy.count()), (0, 2));
        drop(copy);
    });
    assert_eq!(obj.retainCount(), before, "and release");
    let text = description(&strong);
    assert!(text.starts_with('<') && text.contains("PointerArray: 0x"), "{text}");

    // Bare pointers and integers are kept as they are.
    use objc2_foundation::NSPointerFunctionsOptions as Options;
    let opaque = NSPointerArray::pointerArrayWithOptions(Options::OpaqueMemory | Options::OpaquePersonality);
    unsafe { opaque.addPointer(0x1234 as *mut std::ffi::c_void) };
    assert_eq!(opaque.pointerAtIndex(0) as usize, 0x1234);
    let integers = NSPointerArray::pointerArrayWithOptions(Options::OpaqueMemory | Options::IntegerPersonality);
    unsafe { integers.addPointer(42 as *mut std::ffi::c_void) };
    assert_eq!(integers.pointerAtIndex(0) as usize, 42);
    let functions = NSPointerFunctions::pointerFunctionsWithOptions(Options::WeakMemory);
    let with_functions = NSPointerArray::pointerArrayWithPointerFunctions(&functions);
    assert!(with_functions.pointerFunctions().usesWeakReadAndWriteBarriers());
    let old: Retained<NSPointerArray> =
        unsafe { Retained::cast_unchecked(NSPointerArray::pointerArrayWithWeakObjects()) };
    assert_eq!(old.count(), 0);
}

// NSCountedSet

fn counted(items: &[i64]) -> Retained<NSCountedSet<NSNumber>> {
    NSCountedSet::initWithArray(NSCountedSet::alloc(), &numbers(items))
}

#[test]
fn counted_sets() {
    let set = NSCountedSet::<NSNumber>::new();
    set.addObject(&n(1));
    set.addObject(&n(1));
    set.addObject(&n(2));
    assert_eq!(set.count(), 2, "distinct members");
    assert_eq!((set.countForObject(&n(1)), set.countForObject(&n(2)), set.countForObject(&n(3))), (2, 1, 0));
    assert!(set.containsObject(&n(2)));
    assert_eq!(set.member(&n(1)).unwrap().as_i64(), 1);
    assert_eq!(
        description(&set.allObjects().sortedArrayUsingDescriptors(&NSArray::from_retained_slice(&[
            NSSortDescriptor::sortDescriptorWithKey_ascending(None, true)
        ]))),
        "(\n    1,\n    2\n)"
    );
    let text = description(&set);
    assert!(text == "{(\n    1,\n    2\n)}" || text == "{(\n    2,\n    1\n)}", "{text}");
    set.removeObject(&n(1));
    assert_eq!((set.countForObject(&n(1)), set.count()), (1, 2));
    set.removeObject(&n(1));
    assert_eq!((set.countForObject(&n(1)), set.count()), (0, 1));
    set.removeObject(&n(42));
    assert_eq!(set.count(), 1);

    let three = counted(&[1, 1, 1]);
    assert_eq!(three.countForObject(&n(1)), 3);
    let copy = three.copy();
    assert!(copy.isKindOfClass(NSCountedSet::<AnyObject>::class()));
    let copy: Retained<NSCountedSet<NSNumber>> = unsafe { Retained::cast_unchecked(copy) };
    assert_eq!(copy.countForObject(&n(1)), 3, "copies keep the counts");
    let mutable_copy = three.mutableCopy();
    assert!(mutable_copy.isKindOfClass(NSCountedSet::<AnyObject>::class()));
    three.addObject(&n(1));
    assert_eq!(copy.countForObject(&n(1)), 3, "and don't follow changes");

    // Equality counts.
    assert!(counted(&[1, 1, 2]).isEqual(Some(&counted(&[2, 1, 1]))));
    assert!(!counted(&[1, 1, 2]).isEqual(Some(&counted(&[1, 2]))));
    assert!(!three.isEqual(Some(&NSSet::from_retained_slice(&[n(1)]))));
    assert!(!counted(&[1, 1, 2]).isEqualToSet(&NSSet::from_retained_slice(&[n(1), n(2)])));

    // Set algebra, one count at a time.
    let set = counted(&[1, 1]);
    set.unionSet(&NSSet::from_retained_slice(&[n(1), n(5)]));
    assert_eq!((set.countForObject(&n(1)), set.countForObject(&n(5))), (3, 1));
    set.minusSet(&NSSet::from_retained_slice(&[n(1)]));
    assert_eq!(set.countForObject(&n(1)), 2);
    set.addObjectsFromArray(&numbers(&[1, 1]));
    assert_eq!(set.countForObject(&n(1)), 4);
    set.intersectSet(&NSSet::from_retained_slice(&[n(1)]));
    assert_eq!((set.countForObject(&n(1)), set.count()), (1, 1));
    set.setSet(&NSSet::from_retained_slice(&[n(1), n(2)]));
    assert_eq!((set.countForObject(&n(1)), set.countForObject(&n(2))), (1, 1));
    set.removeAllObjects();
    assert_eq!(set.count(), 0);
    let from_set = NSCountedSet::initWithSet(NSCountedSet::alloc(), &NSSet::from_retained_slice(&[n(1)]));
    assert_eq!(from_set.countForObject(&n(1)), 1);

    // Enumeration hands out each member once.
    let set = counted(&[3, 3, 4]);
    let mut seen = fast_numbers(&*set);
    seen.sort();
    assert_eq!(seen, [3, 4]);
    let mut seen: Vec<i64> = unsafe { set.objectEnumerator() }.allObjects().iter().map(|x| x.as_i64()).collect();
    seen.sort();
    assert_eq!(seen, [3, 4]);
    let e = unsafe { set.objectEnumerator() };
    let _ = e.nextObject();
    set.addObject(&n(9));
    assert!(failure(|| while e.nextObject().is_some() {}).ends_with("was mutated while being enumerated."));

    let reason = failure(|| unsafe {
        let _: () = msg_send![&*set, addObject: std::ptr::null::<AnyObject>()];
    });
    assert!(reason.ends_with("-[NSCountedSet addObject:]: attempt to insert nil"), "{reason}");
    let reason = failure(|| unsafe {
        let _: () = msg_send![&*set, removeObject: std::ptr::null::<AnyObject>()];
    });
    assert!(reason.ends_with("-[NSCountedSet removeObject:]: attempt to remove nil"), "{reason}");
    let none: NSUInteger = unsafe { msg_send![&*set, countForObject: std::ptr::null::<AnyObject>()] };
    assert_eq!(none, 0);
}

// NSCache

thread_local!(static EVICTED: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) });

fn evicted() -> Vec<i64> {
    EVICTED.with(|e| e.take())
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "Collections2CacheDelegate"]
    struct CacheDelegate;

    unsafe impl NSObjectProtocol for CacheDelegate {}

    unsafe impl NSCacheDelegate for CacheDelegate {
        #[unsafe(method(cache:willEvictObject:))]
        fn will_evict(&self, _cache: &NSCache, object: &AnyObject) {
            let value = object.downcast_ref::<NSNumber>().map_or(-1, |n| n.as_i64());
            EVICTED.with(|e| e.borrow_mut().push(value));
        }
    }
);

impl CacheDelegate {
    fn new() -> Retained<Self> {
        unsafe { msg_send![Self::alloc(), init] }
    }
}

#[test]
fn caches() {
    let cache: Retained<NSCache<NSString, NSNumber>> = unsafe { NSCache::new() };
    let delegate = CacheDelegate::new();
    let get =
        |cache: &NSCache<NSString, NSNumber>, key: &str| unsafe { cache.objectForKey(&s(key)) }.map(|v| v.as_i64());
    unsafe {
        assert_eq!(cache.name().to_string(), "");
        cache.setName(&s("images"));
        assert_eq!(cache.name().to_string(), "images");
        assert!(cache.evictsObjectsWithDiscardedContent());
        assert_eq!((cache.countLimit(), cache.totalCostLimit()), (0, 0));
        cache.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        assert!(cache.delegate().is_some());

        cache.setObject_forKey(&n(1), &s("a"));
        assert_eq!(get(&cache, "a"), Some(1));
        assert_eq!(get(&cache, "b"), None);

        // Past the count limit, objects go, and the delegate hears of it.
        cache.setCountLimit(2);
        assert_eq!(cache.countLimit(), 2);
        cache.setObject_forKey(&n(2), &s("b"));
        cache.setObject_forKey(&n(3), &s("c"));
        let present: Vec<_> = ["a", "b", "c"].iter().filter_map(|k| get(&cache, k)).collect();
        assert!(present.len() <= 2 && present.contains(&3), "{present:?}");
        assert_eq!(evicted().len(), 3 - present.len());

        // Replacing an object lets the old one go.
        cache.setObject_forKey(&n(30), &s("c"));
        assert_eq!(get(&cache, "c"), Some(30));
        assert_eq!(evicted(), [3]);
        // So does removing it.
        cache.removeObjectForKey(&s("c"));
        assert_eq!(get(&cache, "c"), None);
        assert_eq!(evicted(), [30]);
        cache.removeObjectForKey(&s("nothing"));
        assert_eq!(evicted(), [] as [i64; 0]);
        cache.removeAllObjects();
        assert!(evicted().len() <= 1);
        assert_eq!(get(&cache, "a").or(get(&cache, "b")), None);

        // Costs.
        cache.setCountLimit(0);
        cache.setTotalCostLimit(10);
        cache.setObject_forKey_cost(&n(1), &s("a"), 6);
        cache.setObject_forKey_cost(&n(2), &s("b"), 6);
        assert_eq!(get(&cache, "b"), Some(2));
        assert_eq!(get(&cache, "a"), None);
        assert_eq!(evicted(), [1]);
        // An object costing more than the limit doesn't stay.
        cache.setObject_forKey_cost(&n(3), &s("big"), 20);
        assert_eq!(get(&cache, "big"), None);
        assert!(evicted().contains(&3));

        // Nil: a key is ignored, an object fails.
        let reason = failure(|| {
            let _: () = msg_send![&*cache, setObject: std::ptr::null::<AnyObject>(), forKey: &*s("k")];
        });
        assert!(reason.ends_with("attempt to insert nil value (key: k)"), "{reason}");
        let _: () = msg_send![&*cache, setObject: &*n(1), forKey: std::ptr::null::<AnyObject>()];
        let nothing: *mut AnyObject = msg_send![&*cache, objectForKey: std::ptr::null::<AnyObject>()];
        assert!(nothing.is_null());
        let _: () = msg_send![&*cache, removeObjectForKey: std::ptr::null::<AnyObject>()];

        // What the cache holds when it goes is let go, and told of.
        cache.setTotalCostLimit(0);
        cache.setObject_forKey(&n(5), &s("e"));
        let _ = evicted();
        drop(cache);
        assert_eq!(evicted(), [5]);
    }
}

#[test]
fn caches_are_thread_safe() {
    let cache: Retained<NSCache<NSNumber, NSNumber>> = unsafe { NSCache::new() };
    unsafe { cache.setCountLimit(100) };
    struct Shared(Retained<NSCache<NSNumber, NSNumber>>);
    // SAFETY: NSCache is documented as safe to use from several threads.
    unsafe impl Sync for Shared {}
    let shared = Shared(cache);
    std::thread::scope(|scope| {
        for t in 0..8i64 {
            let shared = &shared;
            scope.spawn(move || {
                autoreleasepool(|_| {
                    for i in 0..2000i64 {
                        let key = n((i * 7 + t) % 300);
                        unsafe {
                            match i % 4 {
                                0 | 1 => shared.0.setObject_forKey(&n(i), &key),
                                2 => {
                                    let _ = shared.0.objectForKey(&key);
                                }
                                _ => shared.0.removeObjectForKey(&key),
                            }
                        }
                    }
                });
            });
        }
    });
    let cache = shared.0;
    let present = autoreleasepool(|_| (0..300).filter(|&k| unsafe { cache.objectForKey(&n(k)) }.is_some()).count());
    // Foundation calls its limits imprecise; Sidestep's are exact.
    assert!(present <= 300, "{present}");
    #[cfg(not(target_vendor = "apple"))]
    assert!(present <= 100, "{present}");
}

// Weak tables and objects that go on their own

struct Observers {
    table: Retained<NSHashTable<AnyObject>>,
    map: Retained<NSMapTable<AnyObject, NSNumber>>,
}

define_class!(
    /// An observer that takes itself out of the tables it's in as it
    /// deallocates, as observers do.
    #[unsafe(super(NSObject))]
    #[name = "Collections2Observer"]
    #[ivars = std::rc::Rc<Observers>]
    struct Observer;
);

impl Drop for Observer {
    fn drop(&mut self) {
        let this: &AnyObject = self;
        self.ivars().table.removeObject(Some(this));
        self.ivars().map.removeObjectForKey(Some(this));
    }
}

impl Observer {
    fn new(observers: &std::rc::Rc<Observers>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(observers.clone());
        unsafe { msg_send![super(this), init] }
    }
}

#[test]
fn weak_members_may_leave_as_they_go() {
    let observers = std::rc::Rc::new(Observers {
        table: NSHashTable::weakObjectsHashTable(),
        map: NSMapTable::weakToStrongObjectsMapTable(),
    });
    let kept = Observer::new(&observers);
    autoreleasepool(|_| {
        observers.table.addObject(Some(&kept));
        observers.map.setObject_forKey(Some(&n(0)), Some(&kept));
        for i in 0..20 {
            let observer = Observer::new(&observers);
            observers.table.addObject(Some(&observer));
            observers.map.setObject_forKey(Some(&n(i)), Some(&observer));
        }
    });
    autoreleasepool(|_| {
        assert_eq!(observers.table.allObjects().count(), 1);
        assert_eq!(unsafe { observers.map.keyEnumerator() }.allObjects().count(), 1);
        assert!(observers.table.containsObject(Some(&kept)));
        assert_eq!(observers.map.objectForKey(Some(&kept)).unwrap().as_i64(), 0);
    });
}

#[test]
fn weak_tables_while_objects_go_on_other_threads() {
    struct Shared(Retained<NSHashTable<Token>>, Retained<NSMapTable<Token, NSNumber>>);
    // SAFETY: the tables are only read here; other threads only release
    // objects the tables hold weakly.
    unsafe impl Sync for Shared {}
    unsafe impl Send for Shared {}
    let shared = Shared(NSHashTable::weakObjectsHashTable(), NSMapTable::weakToStrongObjectsMapTable());
    let keep: Vec<_> = (0..64).map(Token::new).collect();
    autoreleasepool(|_| {
        for t in &keep {
            shared.0.addObject(Some(t));
            shared.1.setObject_forKey(Some(&n(*t.ivars())), Some(t));
        }
    });
    // Objects added here, released on other threads while this one reads.
    struct Sendable(Retained<Token>);
    unsafe impl Send for Sendable {}
    for round in 0..20 {
        let going: Vec<Sendable> = (0..64).map(|i| Sendable(Token::new(1000 + round * 64 + i))).collect();
        autoreleasepool(|_| {
            for t in &going {
                shared.0.addObject(Some(&t.0));
                shared.1.setObject_forKey(Some(&n(0)), Some(&t.0));
            }
        });
        std::thread::scope(|scope| {
            scope.spawn(move || drop(going));
            autoreleasepool(|_| {
                for t in &keep {
                    assert!(shared.0.containsObject(Some(t)));
                    assert_eq!(shared.1.objectForKey(Some(t)).unwrap().as_i64(), *t.ivars());
                }
                let live = shared.0.allObjects().count();
                assert!(live >= 64, "{live}");
            });
        });
    }
    autoreleasepool(|_| {
        assert_eq!(shared.0.allObjects().count(), 64);
        assert_eq!(unsafe { shared.1.keyEnumerator() }.allObjects().count(), 64);
    });
    #[cfg(not(target_vendor = "apple"))]
    assert_eq!((shared.0.count(), shared.1.count()), (64, 64));
}

#[test]
fn collections_allow_concurrent_readers() {
    struct Shared {
        ordered: Retained<NSOrderedSet<NSNumber>>,
        mutable: Retained<NSMutableOrderedSet<NSNumber>>,
        table: Retained<NSHashTable<NSNumber>>,
        map: Retained<NSMapTable<NSNumber, NSNumber>>,
        counted: Retained<NSCountedSet<NSNumber>>,
    }
    // SAFETY: only read, as Foundation lets collections be.
    unsafe impl Sync for Shared {}
    let items: Vec<i64> = (0..64).collect();
    let shared = Shared {
        ordered: ordered(&items),
        mutable: mutable_ordered(&items),
        table: NSHashTable::hashTableWithOptions(objc2_foundation::NSPointerFunctionsOptions::StrongMemory),
        map: NSMapTable::strongToStrongObjectsMapTable(),
        counted: counted(&items),
    };
    for i in 0..64 {
        shared.table.addObject(Some(&n(i)));
        shared.map.setObject_forKey(Some(&n(i * 2)), Some(&n(i)));
    }
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let shared = &shared;
            scope.spawn(move || {
                autoreleasepool(|_| {
                    for i in 0..5000usize {
                        let k = (i % 64) as i64;
                        let probe = n(k);
                        assert_eq!(shared.ordered.indexOfObject(&probe), k as usize);
                        assert_eq!(shared.mutable.indexOfObject(&probe), k as usize);
                        assert_eq!(shared.mutable.objectAtIndex(k as usize).as_i64(), k);
                        assert!(shared.table.containsObject(Some(&probe)));
                        assert_eq!(shared.map.objectForKey(Some(&probe)).unwrap().as_i64(), k * 2);
                        assert_eq!(shared.counted.countForObject(&probe), 1);
                    }
                });
            });
        }
    });
    shared.mutable.addObject(&n(100));
    assert_eq!(shared.mutable.count(), 65);
}

/// Sidestep's mutable ordered sets tell fast enumeration of every change,
/// as its arrays do; Foundation's don't.
#[cfg(not(target_vendor = "apple"))]
#[test]
fn ordered_set_changes_during_fast_enumeration_are_seen() {
    let m = mutable_ordered(&[1, 2, 3]);
    let mut state = NSFastEnumerationState {
        state: 0,
        itemsPtr: std::ptr::null_mut(),
        mutationsPtr: std::ptr::null_mut(),
        extra: [0; 5],
    };
    let mut buffer = [std::ptr::null_mut::<AnyObject>(); 4];
    let got = unsafe {
        m.countByEnumeratingWithState_objects_count(
            NonNull::from(&mut state),
            NonNull::new(buffer.as_mut_ptr()).unwrap(),
            4,
        )
    };
    assert_eq!(got, 3);
    let before = unsafe { *state.mutationsPtr };
    m.addObject(&n(4));
    assert_ne!(unsafe { *state.mutationsPtr }, before);
    let table = NSHashTable::<NSNumber>::weakObjectsHashTable();
    let mut state = NSFastEnumerationState {
        state: 0,
        itemsPtr: std::ptr::null_mut(),
        mutationsPtr: std::ptr::null_mut(),
        extra: [0; 5],
    };
    let kept = n(1000);
    table.addObject(Some(&kept));
    let got = unsafe {
        table.countByEnumeratingWithState_objects_count(
            NonNull::from(&mut state),
            NonNull::new(buffer.as_mut_ptr()).unwrap(),
            4,
        )
    };
    assert_eq!(got, 1);
    let before = unsafe { *state.mutationsPtr };
    table.addObject(Some(&n(1001)));
    assert_ne!(unsafe { *state.mutationsPtr }, before);
}
