//! `NSArray`, `NSMutableArray`, `NSSet`, `NSNumber` and the other
//! collections' (`NSOrderedSet`, sort descriptors, `NSHashTable`,
//! `NSMapTable`, `NSCountedSet`, `NSPointerArray`, `NSCache`) costs, in
//! nanoseconds per operation (per element for whole-array loops). Run in
//! release mode on macOS (Apple's Foundation) and on Linux (Sidestep's) to
//! compare: `cargo run --release -p arraybench`, or with `-- follow-on`
//! for the other collections alone.
//!
//! Each figure is the median of seven timed runs after a warm-up. Strings
//! are long enough that Apple doesn't store them as tagged pointers, so
//! "equal" searches really compare distinct objects. Apple stores small
//! numbers as tagged pointers, which makes creating them nearly free there.

use std::hint::black_box;
use std::ptr::NonNull;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSCache, NSComparisonResult, NSCountedSet, NSDictionary, NSEnumerationOptions, NSHashTable, NSIndexSet,
    NSMapTable, NSMutableArray, NSMutableDictionary, NSMutableIndexSet, NSMutableOrderedSet, NSMutableSet, NSNumber,
    NSOrderedSet, NSPointerArray, NSRange, NSSet, NSSortDescriptor, NSString, NSValue,
};

use sidestep as _;

fn text(i: usize) -> Retained<NSString> {
    NSString::from_str(&format!("array-element-{i:05}-with-some-length"))
}

/// Median of seven timed runs of `iters` calls, after a warm-up, in
/// batches inside autorelease pools. `per` divides the result, for loops
/// that handle many elements per call.
fn bench(name: &str, iters: usize, per: usize, mut f: impl FnMut(usize)) {
    let run = |f: &mut dyn FnMut(usize), n: usize| {
        let mut i = 0;
        while i < n {
            autoreleasepool(|_| {
                for _ in 0..1000.min(n - i) {
                    f(i);
                    i += 1;
                }
            });
        }
    };
    run(&mut f, iters / 10);
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = Instant::now();
            run(&mut f, iters);
            start.elapsed().as_nanos() as f64 / (iters * per) as f64
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    println!("{name:<44} {:>9.2} ns", runs[3]);
}

fn main() {
    // `-- follow-on` runs only the collections of the follow-on.
    if std::env::args().nth(1).as_deref() == Some("follow-on") {
        others();
        ordered_set_changes();
        key_values();
        return;
    }
    for n in [8, 1024] {
        let items: Vec<Retained<NSString>> = (0..n).map(text).collect();
        let equal: Vec<Retained<NSString>> = (0..n).map(text).collect();
        let refs: Vec<&NSString> = items.iter().map(|s| &**s).collect();
        let array = NSArray::from_slice(&refs);
        let mutable = NSMutableArray::from_slice(&refs);
        println!("{n} elements");

        bench("  build NSArray from a slice", (400_000 / n).max(500), 1, |_| {
            black_box(NSArray::from_slice(black_box(&refs)));
        });
        bench("  count", 20_000_000, 1, |_| {
            black_box(black_box(&array).count());
        });
        bench("  objectAtIndex:", 20_000_000, 1, |i| {
            black_box(black_box(&array).objectAtIndex(i % n));
        });
        bench("  objectAtIndex:, mutable", 20_000_000, 1, |i| {
            black_box(black_box(&mutable).objectAtIndex(i % n));
        });
        bench("  for-in (objc2 iter), per element", (4_000_000 / n).max(500), n, |_| {
            for x in black_box(&array).iter() {
                black_box(x);
            }
        });
        bench("  for-in, mutable, per element", (4_000_000 / n).max(500), n, |_| {
            for x in black_box(&mutable).iter() {
                black_box(x);
            }
        });
        let block = RcBlock::new(|obj: NonNull<NSString>, _i: usize, _stop: NonNull<Bool>| {
            black_box(obj);
        });
        bench("  enumerateObjectsUsingBlock:, per element", (4_000_000 / n).max(500), n, |_| {
            black_box(&array).enumerateObjectsUsingBlock(&block);
        });
        bench("  containsObject:, equal string, last", (2_000_000 / n).max(200), 1, |_| {
            black_box(black_box(&array).containsObject(&equal[n - 1]));
        });
        bench("  indexOfObject:, same object, last", (2_000_000 / n).max(200), 1, |_| {
            black_box(black_box(&array).indexOfObject(&items[n - 1]));
        });
        bench("  isEqualToArray:, equal strings", (2_000_000 / n).max(200), 1, |_| {
            black_box(black_box(&array).isEqualToArray(&mutable));
        });
        bench("  copy of a mutable array", (400_000 / n).max(500), 1, |_| {
            black_box(objc2_foundation::NSCopying::copy(black_box(&*mutable)));
        });
        let compare = RcBlock::new(|a: NonNull<AnyObject>, b: NonNull<AnyObject>| {
            let (a, b) = unsafe { (a.cast::<NSNumber>().as_ref(), b.cast::<NSNumber>().as_ref()) };
            a.compare(b)
        });
        let unsorted: Vec<Retained<NSNumber>> =
            (0..n).map(|i| NSNumber::new_i64(((i * 7919) % n) as i64 + 1_000_000)).collect();
        let unsorted = NSArray::from_retained_slice(&unsorted);
        bench("  sortedArrayUsingComparator:, per element", (400_000 / n).max(200), n, |_| {
            black_box(unsafe { black_box(&unsorted).sortedArrayUsingComparator(RcBlock::as_ptr(&compare)) });
        });
        let set = NSSet::from_slice(&refs);
        bench("  NSSet containsObject:, equal string", 10_000_000, 1, |i| {
            black_box(black_box(&set).containsObject(&equal[i % n]));
        });
    }

    println!("mutation");
    let one = text(1);
    let m = NSMutableArray::<NSString>::new();
    bench("  addObject: + removeLastObject", 10_000_000, 1, |_| {
        m.addObject(&one);
        m.removeLastObject();
    });
    bench("  insertObject:atIndex:0 + removeObjectAtIndex:0 (8)", 10_000_000, 1, |_| {
        if m.count() < 8 {
            for _ in 0..8 {
                m.addObject(&one);
            }
        }
        m.insertObject_atIndex(&one, 0);
        m.removeObjectAtIndex(0);
    });
    bench("  NSMutableArray new + 16 addObject:", 500_000, 1, |_| {
        let a = NSMutableArray::<NSString>::new();
        for _ in 0..16 {
            a.addObject(&one);
        }
        black_box(a);
    });
    for n in [1024, 100_000] {
        let queue = NSMutableArray::<NSString>::new();
        for _ in 0..n {
            queue.addObject(&one);
        }
        bench(&format!("  queue of {n}: add, remove at 0"), 2_000_000, 1, |_| {
            queue.addObject(&one);
            queue.removeObjectAtIndex(0);
        });
        bench(&format!("  front of {n}: insert at 0, remove at 0"), 2_000_000, 1, |_| {
            queue.insertObject_atIndex(&one, 0);
            queue.removeObjectAtIndex(0);
        });
    }
    let names: Vec<Retained<NSString>> = (0..2000).map(text).collect();
    let halves: Vec<Retained<NSString>> = (0..2000).step_by(2).map(text).collect();
    let all = NSArray::from_retained_slice(&names);
    let half = NSArray::from_retained_slice(&halves);
    bench("  removeObjectsInArray: of 1000 from 2000", 2_000, 1, |_| {
        let m = objc2_foundation::NSMutableCopying::mutableCopy(&*all);
        m.removeObjectsInArray(&half);
        black_box(m);
    });
    let ms = NSMutableSet::<NSString>::new();
    let texts: Vec<Retained<NSString>> = (0..64).map(text).collect();
    bench("  NSMutableSet addObject: + removeObject:", 5_000_000, 1, |i| {
        let t = &texts[i % 64];
        ms.addObject(t);
        ms.removeObject(t);
    });

    println!("other");
    let numbers: Vec<Retained<NSNumber>> = (0..10_000).map(NSNumber::new_i64).collect();
    let numbers = NSArray::from_retained_slice(&numbers);
    let even = RcBlock::new(|x: NonNull<NSNumber>, _i: usize, _stop: NonNull<Bool>| {
        Bool::new(unsafe { x.as_ref() }.as_i64() % 2 == 0)
    });
    bench("  indexesOfObjects… reverse, per element", 200, 10_000, |_| {
        black_box(numbers.indexesOfObjectsWithOptions_passingTest(NSEnumerationOptions::Reverse, &even));
    });
    bench("  makeObjectsPerformSelector:, per element", 2_000, 10_000, |_| unsafe {
        numbers.makeObjectsPerformSelector(objc2::sel!(self));
    });
    let sparse = NSMutableIndexSet::new();
    for i in (0..2000).step_by(2) {
        sparse.addIndex(i);
    }
    bench("  NSIndexSet count, 1000 ranges", 5_000_000, 1, |_| {
        black_box(black_box(&sparse).count());
    });
    let huge = NSIndexSet::indexSetWithIndexesInRange(NSRange::new(0, 100_000_000));
    let stop = RcBlock::new(|_i: usize, stop: NonNull<Bool>| unsafe { *stop.as_ptr() = Bool::YES });
    bench("  enumerateIndexes of 100M, stop at once", 1_000_000, 1, |_| {
        black_box(&huge).enumerateIndexesUsingBlock(&stop);
    });
    let keys: Vec<Retained<NSValue>> =
        (0..64usize).map(|i| unsafe { NSValue::valueWithPointer((i * 16) as *const _) }).collect();
    let probes: Vec<Retained<NSValue>> =
        (0..64usize).map(|i| unsafe { NSValue::valueWithPointer((i * 16) as *const _) }).collect();
    let by_value = NSMutableDictionary::<NSValue, NSNumber>::new();
    for (i, key) in keys.iter().enumerate() {
        by_value.insert(&**key, &NSNumber::new_usize(i));
    }
    bench("  NSValue key lookup, mutable (64)", 5_000_000, 1, |i| {
        black_box(black_box(&by_value).objectForKey(&probes[i % 64]));
    });

    others();
    ordered_set_changes();
    key_values();

    println!("NSNumber");
    bench("  numberWithInteger: small + release", 10_000_000, 1, |i| {
        black_box(NSNumber::new_isize(black_box((i % 100) as isize)));
    });
    bench("  numberWithInteger: large + release", 10_000_000, 1, |i| {
        black_box(NSNumber::new_isize(black_box(i as isize + (1 << 60))));
    });
    bench("  numberWithDouble: + release", 10_000_000, 1, |i| {
        black_box(NSNumber::new_f64(black_box(i as f64 + 0.5)));
    });
    let a = NSNumber::new_i64(1 << 60);
    let b = NSNumber::new_f64((1u64 << 60) as f64);
    bench("  integerValue", 20_000_000, 1, |_| {
        black_box(black_box(&a).as_isize());
    });
    bench("  isEqualToNumber: (int vs double)", 20_000_000, 1, |_| {
        black_box(black_box(&a).isEqualToNumber(&b));
    });
    bench("  compare:", 20_000_000, 1, |_| {
        black_box(black_box(&a).compare(&b) == NSComparisonResult::Same);
    });
    bench("  hash", 20_000_000, 1, |_| {
        black_box(objc2::runtime::NSObjectProtocol::hash(black_box(&*a)));
    });
}

/// The collections of the follow-on: ordered sets, sort descriptors, hash
/// and map tables, counted sets, pointer arrays and caches.
fn others() {
    let n = 1024;
    let items: Vec<Retained<NSString>> = (0..n).map(text).collect();
    let equal: Vec<Retained<NSString>> = (0..n).map(text).collect();
    let array = NSArray::from_retained_slice(&items);
    println!("NSOrderedSet, {n} elements");
    bench("  orderedSetWithArray:", 2_000, 1, |_| {
        black_box(NSOrderedSet::orderedSetWithArray(black_box(&array)));
    });
    let ordered = NSOrderedSet::orderedSetWithArray(&array);
    bench("  indexOfObject:, equal string", 5_000_000, 1, |i| {
        black_box(black_box(&ordered).indexOfObject(&equal[i % n]));
    });
    bench("  containsObject:, equal string", 5_000_000, 1, |i| {
        black_box(black_box(&ordered).containsObject(&equal[i % n]));
    });
    bench("  objectAtIndex:", 20_000_000, 1, |i| {
        black_box(black_box(&ordered).objectAtIndex(i % n));
    });
    let block = RcBlock::new(|obj: NonNull<NSString>, _i: usize, _stop: NonNull<Bool>| {
        black_box(obj);
    });
    bench("  enumerateObjectsUsingBlock:, per element", 4_000, n, |_| {
        black_box(&ordered).enumerateObjectsUsingBlock(&block);
    });
    let mutable = NSMutableOrderedSet::orderedSetWithArray(&array);
    let extra = text(99_999);
    bench("  mutable: addObject: + removeObject: (at the end)", 5_000_000, 1, |_| {
        mutable.addObject(&extra);
        mutable.removeObject(&extra);
    });
    bench("  mutable: indexOfObject:, equal string", 5_000_000, 1, |i| {
        black_box(black_box(&mutable).indexOfObject(&equal[i % n]));
    });

    println!("NSSortDescriptor, {n} elements");
    let unsorted: Vec<Retained<NSNumber>> =
        (0..n).map(|i| NSNumber::new_i64(((i * 7919) % n) as i64 + 1_000_000)).collect();
    let unsorted = NSArray::from_retained_slice(&unsorted);
    let by_value = NSArray::from_retained_slice(&[NSSortDescriptor::sortDescriptorWithKey_ascending(None, true)]);
    bench("  numbers by value, per element", 2_000, n, |_| {
        black_box(black_box(&unsorted).sortedArrayUsingDescriptors(&by_value));
    });
    let key = NSString::from_str("value");
    let records: Vec<Retained<NSDictionary<NSString, NSNumber>>> =
        unsorted.iter().map(|v| NSDictionary::from_retained_objects(&[&*key], &[v])).collect();
    let records = NSArray::from_retained_slice(&records);
    let by_key = NSArray::from_retained_slice(&[NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&key), true)]);
    bench("  dictionaries by key, per element", 1_000, n, |_| {
        black_box(black_box(&records).sortedArrayUsingDescriptors(&by_key));
    });

    println!("NSHashTable and NSMapTable, 64 members");
    let members: Vec<Retained<NSString>> = (0..64).map(text).collect();
    let probes: Vec<Retained<NSString>> = (0..64).map(text).collect();
    let weak = NSHashTable::<NSString>::weakObjectsHashTable();
    for m in &members {
        weak.addObject(Some(m));
    }
    bench("  weak: containsObject:, equal string", 5_000_000, 1, |i| {
        black_box(black_box(&weak).containsObject(Some(&probes[i % 64])));
    });
    bench("  weak: count", 2_000_000, 1, |_| {
        black_box(black_box(&weak).count());
    });
    bench("  weak: addObject: + removeObject:", 2_000_000, 1, |_| {
        weak.addObject(Some(&extra));
        weak.removeObject(Some(&extra));
    });
    let strong =
        NSHashTable::<NSString>::hashTableWithOptions(objc2_foundation::NSPointerFunctionsOptions::StrongMemory);
    for m in &members {
        strong.addObject(Some(m));
    }
    bench("  strong: containsObject:, equal string", 5_000_000, 1, |i| {
        black_box(black_box(&strong).containsObject(Some(&probes[i % 64])));
    });
    let map = NSMapTable::<NSString, NSString>::strongToStrongObjectsMapTable();
    let weak_keys = NSMapTable::<NSString, NSString>::weakToStrongObjectsMapTable();
    for m in &members {
        map.setObject_forKey(Some(m), Some(m));
        weak_keys.setObject_forKey(Some(m), Some(m));
    }
    bench("  map strong-strong: objectForKey:", 5_000_000, 1, |i| {
        black_box(black_box(&map).objectForKey(Some(&probes[i % 64])));
    });
    bench("  map weak-strong: objectForKey:", 5_000_000, 1, |i| {
        black_box(black_box(&weak_keys).objectForKey(Some(&probes[i % 64])));
    });
    bench("  map strong-strong: setObject:forKey: + remove", 2_000_000, 1, |_| {
        map.setObject_forKey(Some(&extra), Some(&extra));
        map.removeObjectForKey(Some(&extra));
    });

    println!("NSCountedSet, NSPointerArray, NSCache");
    let counted = NSCountedSet::<NSString>::new();
    for m in &members {
        counted.addObject(m);
    }
    bench("  counted: addObject: + removeObject:", 5_000_000, 1, |i| {
        let m = &probes[i % 64];
        counted.addObject(m);
        counted.removeObject(m);
    });
    bench("  counted: countForObject:", 5_000_000, 1, |i| {
        black_box(black_box(&counted).countForObject(&probes[i % 64]));
    });
    let pointers = NSPointerArray::weakObjectsPointerArray();
    for m in &members {
        unsafe { pointers.addPointer(Retained::as_ptr(m).cast_mut().cast()) };
    }
    bench("  weak pointer array: pointerAtIndex:", 5_000_000, 1, |i| {
        black_box(black_box(&pointers).pointerAtIndex(i % 64));
    });
    let cache: Retained<NSCache<NSString, NSString>> = unsafe { NSCache::new() };
    for m in &members {
        unsafe { cache.setObject_forKey(m, m) };
    }
    bench("  cache: objectForKey:, hit", 5_000_000, 1, |i| {
        black_box(unsafe { black_box(&cache).objectForKey(&probes[i % 64]) });
    });
    unsafe { cache.setCountLimit(64) };
    let fresh: Vec<Retained<NSString>> = (1000..1128).map(text).collect();
    bench("  cache: setObject:forKey:, evicting", 2_000_000, 1, |i| {
        let m = &fresh[i % 128];
        unsafe { cache.setObject_forKey(m, m) };
    });
}

/// Changes to a large mutable ordered set away from its end: the costs of
/// using one as a queue or as a most-recently-used list, and of the bulk
/// changes.
fn ordered_set_changes() {
    let n = 20_000;
    let items: Vec<Retained<NSString>> = (0..n).map(text).collect();
    let array = NSArray::from_retained_slice(&items);
    let fresh: Vec<Retained<NSString>> = (n..n + 1000).map(text).collect();
    let fresh = NSArray::from_retained_slice(&fresh);
    println!("NSMutableOrderedSet changes, {n} members");
    let set = NSMutableOrderedSet::orderedSetWithArray(&array);
    bench("  removeObjectAtIndex:0 + addObject: (a queue)", 20_000, 1, |_| {
        let first = set.objectAtIndex(0);
        set.removeObjectAtIndex(0);
        set.addObject(&first);
    });
    bench("  insertObject:atIndex:0 + remove the last", 20_000, 1, |_| {
        let last = set.objectAtIndex(n - 1);
        set.removeObjectAtIndex(n - 1);
        set.insertObject_atIndex(&last, 0);
    });
    bench("  removeObject: + addObject:, the middle one", 20_000, 1, |_| {
        let middle = set.objectAtIndex(n / 2);
        set.removeObject(&middle);
        set.addObject(&middle);
    });
    let plain = NSMutableArray::from_retained_slice(&items);
    bench("  (NSMutableArray: the same, the middle one)", 20_000, 1, |_| {
        let middle = plain.objectAtIndex(n / 2);
        plain.removeObjectAtIndex(n / 2);
        plain.addObject(&middle);
    });
    bench("  removeObject: + addObject:, anywhere", 20_000, 1, |i| {
        let any = set.objectAtIndex((i * 7919) % n);
        set.removeObject(&any);
        set.addObject(&any);
    });
    let front = NSIndexSet::indexSetWithIndexesInRange(NSRange::new(0, 1000));
    bench("  insertObjects:atIndexes: 1000 at 0 (+ remove), per object", 20, 1000, |_| {
        set.insertObjects_atIndexes(&fresh, &front);
        set.removeObjectsInRange(NSRange::new(0, 1000));
    });
    let last = NSIndexSet::indexSetWithIndexesInRange(NSRange::new(n - 1000, 1000));
    bench("  moveObjectsAtIndexes: last 1000 to 0, per object", 20, 1000, |_| {
        set.moveObjectsAtIndexes_toIndex(&last, 0);
    });
    let mut pointers: Vec<NonNull<NSString>> = fresh.iter().map(|o| NonNull::from(&*o)).collect();
    let originals: Vec<Retained<NSString>> = (0..1000).map(|i| set.objectAtIndex(i)).collect();
    let mut others: Vec<NonNull<NSString>> = originals.iter().map(|o| NonNull::from(&**o)).collect();
    bench("  replaceObjectsInRange: first 1000, per object", 20, 1000, |_| {
        unsafe { set.replaceObjectsInRange_withObjects_count(NSRange::new(0, 1000), pointers.as_mut_ptr(), 1000) };
        std::mem::swap(&mut pointers, &mut others);
    });
}

define_class!(
    /// A model object with an integer property.
    #[unsafe(super(NSObject))]
    #[name = "ArrayBenchPerson"]
    #[ivars = isize]
    struct Person;

    impl Person {
        #[unsafe(method(age))]
        fn age(&self) -> isize {
            *self.ivars()
        }
    }
);

/// Key-value reads: sorting model objects by a getter, and dictionaries'
/// `-valueForKey:`.
fn key_values() {
    let n = 4096;
    println!("Key-value reads, {n} elements");
    let people: Vec<Retained<Person>> = (0..n)
        .map(|i| {
            let this = Person::alloc().set_ivars(((i * 7919) % n) as isize);
            unsafe { msg_send![super(this), init] }
        })
        .collect();
    let people = NSArray::from_retained_slice(&people);
    let key = NSString::from_str("age");
    let by_age = NSArray::from_retained_slice(&[NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&key), true)]);
    bench("  objects by an NSInteger getter, per element", 200, n, |_| {
        black_box(black_box(&people).sortedArrayUsingDescriptors(&by_age));
    });
    let record = NSDictionary::from_retained_objects(&[&*key], &[NSNumber::new_i64(1)]);
    bench("  NSDictionary valueForKey:", 5_000_000, 1, |_| {
        black_box(black_box(&record).valueForKey(black_box(&key)));
    });
}
