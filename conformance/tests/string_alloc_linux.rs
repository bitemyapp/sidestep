//! Linux only: how many allocations Sidestep's strings make, and the panics
//! out-of-range calls raise. (On macOS these are Apple's implementation
//! details, and out-of-range calls raise Objective-C exceptions.)
#![cfg(not(target_vendor = "apple"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use objc2::msg_send;
use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2_foundation::{NSMutableString, NSRange, NSString, ns_string};

use sidestep as _;

/// Counts allocations made by the current thread.
struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: forwards to the system allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        // SAFETY: forwarded contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        // SAFETY: forwarded contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        // SAFETY: forwarded contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded contract.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations `f` makes. Debug builds of objc2 check every message's
/// signature, which allocates, so counts are only meaningful in release
/// builds; in debug builds this returns `expected` after running `f`.
fn allocations_or(expected: usize, f: impl FnOnce()) -> usize {
    let n = allocations(f);
    if cfg!(debug_assertions) { expected } else { n }
}

fn allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    f();
    ALLOCATIONS.with(Cell::get) - before
}

#[test]
fn short_strings_take_one_allocation() {
    // Load the classes and fill the method caches first.
    for text in ["warm", "wärm"] {
        let s = NSString::from_str(text);
        let _ = (s.length(), s.characterAtIndex(1), s.UTF8String(), s.len());
    }
    for text in ["hello", "héllo wörld", "漢字", "🎉"] {
        assert_eq!(allocations_or(1, || drop(NSString::from_str(text))), 1, "from_str({text:?})");
    }
}

#[test]
fn reading_immutable_strings_allocates_nothing() {
    let s = NSString::from_str("héllo wörld 🎉");
    let _ = (s.length(), s.characterAtIndex(3), s.UTF8String(), s.len(), s.hash());
    let n = allocations_or(0, || {
        for _ in 0..3 {
            let _ = s.length();
            for i in 0..s.length() {
                let _ = s.characterAtIndex(i);
            }
            let _ = s.UTF8String();
            let _ = s.len();
            let _ = s.hash();
        }
        autoreleasepool(|pool| {
            let _ = unsafe { s.to_str(pool) };
        });
    });
    assert_eq!(n, 0);
    let constant = ns_string!("a constant");
    let n = allocations_or(0, || {
        let _ = (constant.length(), constant.characterAtIndex(2), constant.UTF8String());
    });
    assert_eq!(n, 0);
}

fn panic_message(f: impl FnOnce()) -> String {
    let err = catch_unwind(AssertUnwindSafe(f)).expect_err("should panic");
    match err.downcast::<String>() {
        Ok(s) => *s,
        Err(err) => err.downcast::<&str>().map(|s| s.to_string()).unwrap_or_default(),
    }
}

#[test]
fn out_of_range_calls_panic_like_foundation() {
    let s = NSString::from_str("abc");
    assert_eq!(
        panic_message(|| {
            s.characterAtIndex(5);
        }),
        "-[NSString characterAtIndex:]: index 5 out of bounds; string length 3"
    );
    assert_eq!(
        panic_message(|| {
            s.substringWithRange(NSRange::new(2, 2));
        }),
        "-[NSString substringWithRange:]: Range {2, 2} out of bounds; string length 3"
    );
    assert_eq!(
        panic_message(|| {
            s.substringFromIndex(4);
        }),
        "-[NSString substringFromIndex:]: index 4 out of bounds; string length 3"
    );
    let m = NSMutableString::from_str("abc");
    assert_eq!(
        panic_message(|| m.deleteCharactersInRange(NSRange::new(1, 5))),
        "-[NSString deleteCharactersInRange:]: Range {1, 5} out of bounds; string length 3"
    );
    assert_eq!(
        panic_message(|| m.insertString_atIndex(ns_string!("x"), 4)),
        "-[NSString insertString:atIndex:]: index 4 out of bounds; string length 3"
    );
    // A failed edit leaves the string as it was.
    assert_eq!(m.to_string(), "abc");
}

#[test]
fn string_classes_allocate_through_the_placeholder() {
    // `[s class]` names one of Sidestep's own string classes, whose
    // instances only an initializer can lay out: `+alloc` sent to it gives
    // the placeholder, so `[[[s class] alloc] init…]` makes a whole string.
    let long = NSString::from_str("héllo wörld, and then some");
    for s in [&*long, ns_string!("a constant")] {
        let cls = s.class();
        let fresh: Retained<NSString> = unsafe { msg_send![cls, new] };
        assert_eq!((fresh.length(), fresh.to_string()), (0, String::new()));
        let copy: Retained<NSString> = unsafe {
            let a: Allocated<NSString> = msg_send![cls, alloc];
            msg_send![a, initWithString: s]
        };
        assert_eq!(copy.to_string(), s.to_string());
    }
}
