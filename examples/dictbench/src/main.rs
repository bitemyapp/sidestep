//! `NSDictionary` costs, in nanoseconds per operation. Run in release mode
//! on macOS (Apple's Foundation) and on Linux (Sidestep's) to compare:
//! `cargo run --release -p dictbench`.
//!
//! Keys are strings long enough that Apple doesn't store them as tagged
//! pointers, so "equal" lookups really compare distinct objects.

use std::hint::black_box;
use std::time::Instant;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, NSObject};
use objc2_foundation::{NSDictionary, NSString};

use sidestep as _;

fn key(i: usize) -> Retained<NSString> {
    NSString::from_str(&format!("attribute-key-{i:05}-with-some-length"))
}

/// Time `f` over `iters` calls, in batches inside autorelease pools.
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
    let start = Instant::now();
    run(&mut f, iters);
    let ns = start.elapsed().as_nanos() as f64 / iters as f64;
    println!("{name:<40} {ns:>9.1} ns");
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
    }
}
