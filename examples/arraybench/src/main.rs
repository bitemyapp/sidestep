//! `NSArray`, `NSMutableArray`, `NSSet` and `NSNumber` costs, in
//! nanoseconds per operation (per element for whole-array loops). Run in
//! release mode on macOS (Apple's Foundation) and on Linux (Sidestep's) to
//! compare: `cargo run --release -p arraybench`.
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
use objc2::runtime::{AnyObject, Bool};
use objc2_foundation::{NSArray, NSComparisonResult, NSMutableArray, NSMutableSet, NSNumber, NSSet, NSString};

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
    let ms = NSMutableSet::<NSString>::new();
    let texts: Vec<Retained<NSString>> = (0..64).map(text).collect();
    bench("  NSMutableSet addObject: + removeObject:", 5_000_000, 1, |i| {
        let t = &texts[i % 64];
        ms.addObject(t);
        ms.removeObject(t);
    });

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
