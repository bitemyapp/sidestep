//! Cost of the runtime's hot paths, in nanoseconds per operation. Run in
//! release mode on macOS (Apple's runtime) and on Linux (Sidestep's) to
//! compare: `cargo run --release -p msgbench`.

use std::hint::black_box;
use std::time::Instant;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, define_class, msg_send};
use objc2_foundation::NSString;

use sidestep as _;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MsgBenchTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(value))]
        fn value(&self) -> i32 {
            7
        }

        #[unsafe(method(classValue))]
        fn class_value() -> i32 {
            9
        }
    }
);

#[inline(never)]
fn plain_rust(x: i32) -> i32 {
    x.wrapping_add(7)
}

fn bench(name: &str, iters: u64, mut f: impl FnMut()) {
    for _ in 0..iters / 10 {
        f();
    }
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    let ns = start.elapsed().as_nanos() as f64 / iters as f64;
    println!("{name:<44} {ns:>8.2} ns");
}

fn main() {
    let target: Retained<Target> = unsafe { msg_send![Target::alloc(), init] };
    let obj = NSObject::new();
    let f: fn(i32) -> i32 = black_box(plain_rust);

    bench("baseline: indirect call to a Rust fn", 50_000_000, || {
        black_box(f(black_box(1)));
    });
    bench("msg_send instance method (define_class!)", 50_000_000, || {
        let v: i32 = unsafe { msg_send![black_box(&*target), value] };
        black_box(v);
    });
    bench("msg_send class method", 50_000_000, || {
        let v: i32 = unsafe { msg_send![black_box(Target::class()), classValue] };
        black_box(v);
    });
    bench("generated binding: -[NSObject hash]", 50_000_000, || {
        black_box(black_box(&*obj).hash());
    });
    bench("retain + release (Retained clone/drop)", 50_000_000, || {
        drop(black_box(obj.clone()));
    });
    bench("alloc + init + release NSObject", 10_000_000, || {
        drop(black_box(NSObject::new()));
    });
    bench("autorelease pool push/pop, one object", 10_000_000, || {
        autoreleasepool(|pool| {
            let o = unsafe { Retained::autorelease(obj.clone(), pool) };
            black_box(o);
        });
    });
    bench("NSString::from_str(\"hello\") + release", 5_000_000, || {
        drop(black_box(NSString::from_str(black_box("hello"))));
    });

    // Four threads hammering one object's reference count.
    struct Send<T>(T);
    unsafe impl<T> std::marker::Send for Send<T> {}
    unsafe impl<T> Sync for Send<T> {}
    let shared = std::sync::Arc::new(Send(obj.clone()));
    let iters = 10_000_000u64;
    let start = Instant::now();
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let shared = shared.clone();
            std::thread::spawn(move || {
                for _ in 0..iters {
                    drop(black_box(shared.0.clone()));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let ns = start.elapsed().as_nanos() as f64 / iters as f64;
    println!("{:<44} {ns:>8.2} ns", "retain + release, 4 threads contending");
}
