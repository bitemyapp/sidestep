//! `NSDictionary` and `NSMutableDictionary` costs, in nanoseconds per
//! operation (the median of seven runs). Run in release mode on macOS
//! (Apple's Foundation) and on Linux (Sidestep's) to compare:
//! `cargo run --release -p dictbench`.
//!
//! Keys are strings long enough that Apple doesn't store them as tagged
//! pointers, so "equal" lookups really compare distinct objects. Number
//! keys are large enough to be real objects on Linux too.

use std::hint::black_box;
use std::time::Instant;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, NSObject};
use objc2_foundation::{NSCopying, NSDictionary, NSMutableDictionary, NSNumber, NSString};

use sidestep as _;

fn key(i: usize) -> Retained<NSString> {
    NSString::from_str(&format!("attribute-key-{i:05}-with-some-length"))
}

/// Median of seven timed runs of `iters` calls, after a warm-up, in
/// batches inside autorelease pools.
fn bench(name: &str, iters: usize, mut f: impl FnMut(usize)) {
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
            start.elapsed().as_nanos() as f64 / iters as f64
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    println!("{name:<40} {:>9.1} ns", runs[3]);
}

fn main() {
    for n in [2, 8, 64, 1024] {
        let keys: Vec<Retained<NSString>> = (0..n).map(key).collect();
        let equal: Vec<Retained<NSString>> = (0..n).map(key).collect();
        let missing: Vec<Retained<NSString>> = (n..2 * n).map(key).collect();
        let values: Vec<Retained<NSObject>> = (0..n).map(|_| NSObject::new()).collect();
        let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let value_refs: Vec<&AnyObject> = values.iter().map(|v| v.as_ref()).collect();
        let dict = NSDictionary::from_slices(&key_refs, &value_refs);
        assert_eq!(dict.count(), n);
        let number_keys: Vec<Retained<NSNumber>> = (0..n).map(|i| NSNumber::new_i64(i as i64 * 1_000_003)).collect();
        let number_refs: Vec<&NSNumber> = number_keys.iter().map(|k| &**k).collect();
        let numbers = NSDictionary::from_slices(&number_refs, &value_refs);

        println!("{n} entries");
        let build_iters = (200_000 / n).max(200);
        bench("  build", build_iters, |_| {
            black_box(NSDictionary::from_slices(black_box(&key_refs), &value_refs));
        });
        bench("  count", 5_000_000, |_| {
            black_box(black_box(&dict).count());
        });
        bench("  lookup, same key object", 5_000_000, |i| {
            black_box(dict.objectForKey(&keys[i % n]));
        });
        bench("  lookup, equal key", 5_000_000, |i| {
            black_box(dict.objectForKey(&equal[i % n]));
        });
        bench("  lookup, missing key", 5_000_000, |i| {
            black_box(dict.objectForKey(&missing[i % n]));
        });
        bench("  lookup, number key", 5_000_000, |i| {
            black_box(numbers.objectForKey(&number_keys[i % n]));
        });
        bench("  for-in over all the keys", (5_000_000 / n).max(100), |_| {
            for key in black_box(&dict).keys() {
                black_box(key);
            }
        });

        let mutable = NSMutableDictionary::from_slices(&key_refs, &value_refs);
        bench("  mutable: lookup, equal key", 5_000_000, |i| {
            black_box(mutable.objectForKey(&equal[i % n]));
        });
        bench("  mutable: set existing key", 5_000_000, |i| {
            mutable.insert(&*keys[i % n], &*values[i % n]);
        });
        bench("  mutable: set new key + remove it", 2_000_000, |i| {
            let key = &*missing[i % n];
            mutable.insert(key, &*values[0]);
            mutable.removeObjectForKey(key);
        });
        bench("  mutable: build by setObject:forKey:", build_iters, |_| {
            let m = NSMutableDictionary::<NSString, NSObject>::new();
            for (key, value) in keys.iter().zip(&values) {
                m.insert(&**key, value);
            }
            black_box(m);
        });
        bench("  copy of the mutable dictionary", build_iters, |_| {
            black_box(black_box(&*mutable).copy());
        });
    }
}
