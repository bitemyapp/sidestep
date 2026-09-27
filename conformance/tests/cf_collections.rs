//! CoreFoundation's collection functions beyond the basics (services.rs
//! has those): arrays searched, sorted, replaced and applied, dictionaries
//! counted and applied, sets, `CFDataFind`, errors from keys and values,
//! the number constants and a run loop's modes. Runs on macOS against
//! Apple's CoreFoundation and on Linux against Sidestep's.
#![allow(deprecated)]

use std::ffi::c_void;
use std::fmt::Debug;

use objc2_core_foundation::{
    CFArray, CFComparisonResult, CFIndex, CFMutableArray, CFNumber, CFRange, CFRetained, CFString,
    kCFRunLoopDefaultMode, kCFTypeArrayCallBacks,
};
use sidestep as _;

/// Mismatches, collected so that one run shows them all.
#[derive(Default)]
struct Checks(Vec<String>);

impl Checks {
    fn eq<T: PartialEq + Debug>(&mut self, what: &str, got: T, want: T) {
        if got != want {
            self.0.push(format!("{what}: got {got:?}, want {want:?}"));
        }
    }

    fn done(self) {
        assert!(self.0.is_empty(), "{:#?}", self.0);
    }
}

fn range(location: CFIndex, length: CFIndex) -> CFRange {
    CFRange { location, length }
}

fn num(n: i32) -> CFRetained<CFNumber> {
    CFNumber::new_i32(n)
}

fn int(value: *const c_void) -> i32 {
    // SAFETY: the tests' arrays and sets hold numbers.
    unsafe { &*value.cast::<CFNumber>() }.as_i32().unwrap()
}

fn text(value: *const c_void) -> String {
    // SAFETY: a string.
    unsafe { &*value.cast::<CFString>() }.to_string()
}

/// A mutable array of numbers.
fn numbers(values: &[i32]) -> CFRetained<CFMutableArray> {
    let array = unsafe { CFMutableArray::new(None, 0, &kCFTypeArrayCallBacks) }.unwrap();
    for &v in values {
        unsafe { objc2_core_foundation::CFArrayAppendValue(Some(&array), (&*num(v) as *const CFNumber).cast()) };
    }
    array
}

fn ints(array: &CFArray) -> Vec<i32> {
    let n = objc2_core_foundation::CFArrayGetCount(array);
    (0..n).map(|i| int(unsafe { objc2_core_foundation::CFArrayGetValueAtIndex(array, i) })).collect()
}

unsafe extern "C-unwind" fn compare_numbers(
    a: *const c_void,
    b: *const c_void,
    context: *mut c_void,
) -> CFComparisonResult {
    // The context counts the calls.
    if !context.is_null() {
        unsafe { *context.cast::<usize>() += 1 };
    }
    let (a, b) = (int(a), int(b));
    CFComparisonResult(a.cmp(&b) as CFIndex)
}

unsafe extern "C-unwind" fn collect_values(value: *const c_void, context: *mut c_void) {
    unsafe { (*context.cast::<Vec<i32>>()).push(int(value)) };
}

#[test]
fn arrays_search_sort_and_replace() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let array = numbers(&[1, 3, 3, 5, 7, 3]);
    let three = num(3);
    let three = (&*three as *const CFNumber).cast::<c_void>();
    c.eq("count of 3", unsafe { CFArrayGetCountOfValue(&array, range(0, 6), three) }, 3);
    c.eq("count of 3 in 0..2", unsafe { CFArrayGetCountOfValue(&array, range(0, 2), three) }, 1);
    c.eq("last 3", unsafe { CFArrayGetLastIndexOfValue(&array, range(0, 6), three) }, 5);
    c.eq("last 3 in 0..5", unsafe { CFArrayGetLastIndexOfValue(&array, range(0, 5), three) }, 2);
    c.eq("last 3 in 3..5", unsafe { CFArrayGetLastIndexOfValue(&array, range(3, 2), three) }, -1);

    // Applied in order over the range.
    let mut seen: Vec<i32> = Vec::new();
    unsafe { CFArrayApplyFunction(&array, range(1, 3), Some(collect_values), (&raw mut seen).cast()) };
    c.eq("applied", seen, vec![3, 3, 5]);

    // Sorted with a comparator, within the range.
    let sorting = numbers(&[9, 4, 8, 1, 7, 2]);
    let mut calls = 0usize;
    unsafe { CFArraySortValues(Some(&sorting), range(1, 4), Some(compare_numbers), (&raw mut calls).cast()) };
    c.eq("sorted 1..5", ints(&sorting), vec![9, 1, 4, 7, 8, 2]);
    c.eq("comparator called", calls > 0, true);

    // Binary search: a match's index, else where it would go.
    let sorted = numbers(&[1, 3, 5, 7, 9]);
    for (value, want) in [(5, 2), (1, 0), (9, 4), (0, 0), (4, 2), (8, 4), (10, 5)] {
        let v = num(value);
        let got = unsafe {
            CFArrayBSearchValues(
                &sorted,
                range(0, 5),
                (&*v as *const CFNumber).cast(),
                Some(compare_numbers),
                std::ptr::null_mut(),
            )
        };
        c.eq(&format!("bsearch {value}"), got, want);
    }
    let v = num(8);
    let got = unsafe {
        CFArrayBSearchValues(
            &sorted,
            range(1, 2),
            (&*v as *const CFNumber).cast(),
            Some(compare_numbers),
            std::ptr::null_mut(),
        )
    };
    c.eq("bsearch 8 in 1..3", got, 3);

    // Exchange, replace (shrinking, growing, inserting, deleting), append.
    let changing = numbers(&[0, 1, 2, 3, 4]);
    unsafe { CFArrayExchangeValuesAtIndices(Some(&changing), 0, 4) };
    c.eq("exchanged", ints(&changing), vec![4, 1, 2, 3, 0]);
    let (x, y) = (num(10), num(11));
    let mut new_values = [(&*x as *const CFNumber).cast::<c_void>(), (&*y as *const CFNumber).cast::<c_void>()];
    unsafe { CFArrayReplaceValues(Some(&changing), range(1, 3), new_values.as_mut_ptr(), 2) };
    c.eq("replaced 3 with 2", ints(&changing), vec![4, 10, 11, 0]);
    unsafe { CFArrayReplaceValues(Some(&changing), range(4, 0), new_values.as_mut_ptr(), 1) };
    c.eq("appended by replacing", ints(&changing), vec![4, 10, 11, 0, 10]);
    unsafe { CFArrayReplaceValues(Some(&changing), range(0, 2), std::ptr::null_mut(), 0) };
    c.eq("deleted by replacing", ints(&changing), vec![11, 0, 10]);
    let other = numbers(&[20, 21, 22, 23]);
    unsafe { CFArrayAppendArray(Some(&changing), Some(&other), range(1, 2)) };
    c.eq("appended an array's range", ints(&changing), vec![11, 0, 10, 21, 22]);
    c.done();
}

/// A comparator that isn't an order at all: -1, 0 or 1 from a generator
/// in the context.
unsafe extern "C-unwind" fn noisy(_: *const c_void, _: *const c_void, context: *mut c_void) -> CFComparisonResult {
    let state = unsafe { &mut *context.cast::<u64>() };
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    CFComparisonResult((*state >> 33) as CFIndex % 3 - 1)
}

unsafe extern "C-unwind" fn always_less(_: *const c_void, _: *const c_void, _: *mut c_void) -> CFComparisonResult {
    CFComparisonResult::CompareLessThan
}

#[test]
fn sorting_with_comparators_that_arent_orders() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let values: Vec<i32> = (0..500).map(|i| (i * 7919) % 500).collect();
    for (what, comparator) in
        [("noisy", noisy as unsafe extern "C-unwind" fn(_, _, _) -> _), ("always less", always_less)]
    {
        let array = numbers(&values);
        let mut state = 42u64;
        unsafe { CFArraySortValues(Some(&array), range(0, 500), Some(comparator), (&raw mut state).cast()) };
        let mut after = ints(&array);
        c.eq(&format!("{what}: count"), after.len(), 500);
        after.sort();
        c.eq(&format!("{what}: the same values"), after, (0..500).collect::<Vec<_>>());
    }
    c.done();
}

unsafe extern "C-unwind" fn collect_pairs(key: *const c_void, value: *const c_void, context: *mut c_void) {
    unsafe { (*context.cast::<Vec<(String, i32)>>()).push((text(key), int(value))) };
}

#[test]
fn dictionaries_count_and_apply() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let (a, b, z) = (CFString::from_str("a"), CFString::from_str("b"), CFString::from_str("z"));
    let (one, two) = (num(1), num(2));
    let dict = CFDictionary::<CFString, CFNumber>::from_slices(&[&a, &b], &[&one, &one]);
    let dict: &CFDictionary = dict.as_opaque();
    let key = |s: &CFString| (s as *const CFString).cast::<c_void>();
    let value = |n: &CFNumber| (n as *const CFNumber).cast::<c_void>();
    c.eq("count of key a", unsafe { CFDictionaryGetCountOfKey(dict, key(&a)) }, 1);
    c.eq("count of key z", unsafe { CFDictionaryGetCountOfKey(dict, key(&z)) }, 0);
    c.eq("count of value 1", unsafe { CFDictionaryGetCountOfValue(dict, value(&one)) }, 2);
    c.eq("count of value 2", unsafe { CFDictionaryGetCountOfValue(dict, value(&two)) }, 0);
    c.eq("contains 1", unsafe { CFDictionaryContainsValue(dict, value(&one)) }, true);
    c.eq("contains 2", unsafe { CFDictionaryContainsValue(dict, value(&two)) }, false);
    // Equal, not only identical, values count.
    let other_one = num(1);
    c.eq("contains an equal 1", unsafe { CFDictionaryContainsValue(dict, value(&other_one)) }, true);
    let mut pairs: Vec<(String, i32)> = Vec::new();
    unsafe { CFDictionaryApplyFunction(dict, Some(collect_pairs), (&raw mut pairs).cast()) };
    pairs.sort();
    c.eq("applied", pairs, vec![("a".into(), 1), ("b".into(), 1)]);
    c.done();
}

unsafe extern "C-unwind" fn collect_set(value: *const c_void, context: *mut c_void) {
    unsafe { (*context.cast::<Vec<i32>>()).push(int(value)) };
}

#[test]
fn sets() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let (one, two, three) = (num(1), num(2), num(3));
    let p = |n: &CFNumber| (n as *const CFNumber).cast::<c_void>();
    let mut values = [p(&one), p(&two), p(&two), p(&three)];
    let set = unsafe { CFSetCreate(None, values.as_mut_ptr(), 4, &kCFTypeSetCallBacks) }.unwrap();
    let set: &CFSet = &set;
    c.eq("type", CFGetTypeID(Some(set)), CFSet::type_id());
    c.eq("count", CFSetGetCount(set), 3);
    c.eq("count of 2", unsafe { CFSetGetCountOfValue(set, p(&two)) }, 1);
    let four = num(4);
    c.eq("count of 4", unsafe { CFSetGetCountOfValue(set, p(&four)) }, 0);
    c.eq("contains 3", unsafe { CFSetContainsValue(set, p(&three)) }, true);
    let other_two = num(2);
    // The member equal to the candidate, not the candidate.
    let got = unsafe { CFSetGetValue(set, p(&other_two)) };
    c.eq("get 2", int(got), 2);
    c.eq("get 4", unsafe { CFSetGetValue(set, p(&four)) }.is_null(), true);
    let mut found: *const c_void = std::ptr::null();
    c.eq("get if present 3", unsafe { CFSetGetValueIfPresent(set, p(&three), &mut found) }, true);
    c.eq("found 3", int(found), 3);
    c.eq("get if present 4", unsafe { CFSetGetValueIfPresent(set, p(&four), &mut found) }, false);
    let mut all = [std::ptr::null::<c_void>(); 3];
    unsafe { CFSetGetValues(set, all.as_mut_ptr()) };
    let mut all: Vec<i32> = all.iter().map(|&v| int(v)).collect();
    all.sort();
    c.eq("values", all, vec![1, 2, 3]);
    let mut applied: Vec<i32> = Vec::new();
    unsafe { CFSetApplyFunction(set, Some(collect_set), (&raw mut applied).cast()) };
    applied.sort();
    c.eq("applied", applied, vec![1, 2, 3]);

    // Other callbacks than the CFType ones: strings copied as keys are.
    let (s, t) = (CFString::from_str("s"), CFString::from_str("t"));
    let mut strings = [(&*s as *const CFString).cast::<c_void>(), (&*t as *const CFString).cast::<c_void>()];
    let copied = unsafe { CFSetCreate(None, strings.as_mut_ptr(), 2, &kCFCopyStringSetCallBacks) };
    let copied = copied.unwrap();
    c.eq("copy-string set count", CFSetGetCount(&copied), 2);
    // Such a set holds copies: a mutable string changed after it went in
    // leaves the set's member as it was.
    let text = |v: *const c_void| unsafe { &*v.cast::<CFString>() }.to_string();
    let m = CFStringCreateMutable(None, 0).unwrap();
    CFStringAppend(Some(&m), Some(&CFString::from_str("m")));
    let mp = (&*m as *const CFMutableString).cast::<c_void>();
    let mut made_from = [mp];
    let created = unsafe { CFSetCreate(None, made_from.as_mut_ptr(), 1, &kCFCopyStringSetCallBacks) }.unwrap();
    let adding = unsafe { CFSetCreateMutable(None, 0, &kCFCopyStringSetCallBacks) }.unwrap();
    unsafe { CFSetAddValue(Some(&adding), mp) };
    let setting = unsafe { CFSetCreateMutable(None, 0, &kCFCopyStringSetCallBacks) }.unwrap();
    unsafe { CFSetSetValue(Some(&setting), mp) };
    let replacing = unsafe { CFSetCreateMutableCopy(None, 0, Some(&created)) }.unwrap();
    unsafe { CFSetReplaceValue(Some(&replacing), mp) };
    let later = unsafe { CFSetCreateMutableCopy(None, 0, Some(&created)) }.unwrap();
    let n = CFStringCreateMutable(None, 0).unwrap();
    CFStringAppend(Some(&n), Some(&CFString::from_str("n")));
    let np = (&*n as *const CFMutableString).cast::<c_void>();
    unsafe { CFSetAddValue(Some(&later), np) };
    CFStringAppend(Some(&m), Some(&CFString::from_str("x")));
    CFStringAppend(Some(&n), Some(&CFString::from_str("x")));
    let (plain_m, plain_mx) = (CFString::from_str("m"), CFString::from_str("mx"));
    let (pm, pmx) = ((&*plain_m as *const CFString).cast::<c_void>(), (&*plain_mx as *const CFString).cast::<c_void>());
    for (what, set) in [("created", &*created), ("added", &adding), ("set", &setting), ("replaced", &replacing)] {
        let member = unsafe { CFSetGetValue(set, pm) };
        c.eq(&format!("copy-string set {what}: member"), (!member.is_null()).then(|| text(member)), Some("m".into()));
        c.eq(&format!("copy-string set {what}: a copy"), member == mp, false);
        c.eq(&format!("copy-string set {what}: changed string absent"), unsafe { CFSetContainsValue(set, pmx) }, false);
    }
    let (plain_n, plain_nx) = (CFString::from_str("n"), CFString::from_str("nx"));
    let later_n = unsafe { CFSetGetValue(&later, (&*plain_n as *const CFString).cast()) };
    c.eq("copy-string set's mutable copy copies too", (!later_n.is_null()).then(|| text(later_n)), Some("n".into()));
    c.eq(
        "copy-string set's mutable copy: changed string absent",
        unsafe { CFSetContainsValue(&later, (&*plain_nx as *const CFString).cast()) },
        false,
    );

    // Mutable sets: add (if absent), replace (if present), set (either),
    // remove, remove all; copies.
    let mutable = unsafe { CFSetCreateMutable(None, 0, &kCFTypeSetCallBacks) };
    let mutable: CFRetained<CFMutableSet> = mutable.unwrap();
    unsafe { CFSetAddValue(Some(&mutable), p(&one)) };
    unsafe { CFSetAddValue(Some(&mutable), p(&one)) };
    unsafe { CFSetReplaceValue(Some(&mutable), p(&two)) };
    c.eq("add twice, replace absent", CFSetGetCount(&mutable), 1);
    unsafe { CFSetSetValue(Some(&mutable), p(&two)) };
    unsafe { CFSetSetValue(Some(&mutable), p(&other_two)) };
    c.eq("set twice", CFSetGetCount(&mutable), 2);
    // Replacing puts the new object in.
    unsafe { CFSetReplaceValue(Some(&mutable), p(&other_two)) };
    c.eq("replaced", unsafe { CFSetGetValue(&mutable, p(&two)) } == p(&other_two), true);
    let copy = CFSetCreateCopy(None, Some(&mutable));
    let copy = copy.unwrap();
    let mutable_copy = unsafe { CFSetCreateMutableCopy(None, 0, Some(&mutable)) };
    let mutable_copy: CFRetained<CFMutableSet> = mutable_copy.unwrap();
    unsafe { CFSetRemoveValue(Some(&mutable), p(&one)) };
    c.eq("removed", CFSetGetCount(&mutable), 1);
    CFSetRemoveAllValues(Some(&mutable));
    c.eq("removed all", CFSetGetCount(&mutable), 0);
    c.eq("copy kept", CFSetGetCount(&copy), 2);
    unsafe { CFSetAddValue(Some(&mutable_copy), p(&three)) };
    c.eq("mutable copy grew", CFSetGetCount(&mutable_copy), 3);
    c.eq("copies are equal to themselves", CFEqual(Some(&*copy), Some(&*copy)), true);
    c.done();
}

#[test]
fn data_find() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let data = CFData::from_bytes(b"abcabcab");
    let find = |needle: &[u8], r: CFRange, flags: CFDataSearchFlags| {
        let needle = CFData::from_bytes(needle);
        let found = unsafe { CFDataFind(&data, Some(&needle), r, flags) };
        (found.location, found.length)
    };
    let none = CFDataSearchFlags::empty();
    c.eq("abc", find(b"abc", range(0, 8), none), (0, 3));
    c.eq("abc from 1", find(b"abc", range(1, 7), none), (3, 3));
    c.eq("abc backwards", find(b"abc", range(0, 8), CFDataSearchFlags::Backwards), (3, 3));
    c.eq("ab backwards", find(b"ab", range(0, 8), CFDataSearchFlags::Backwards), (6, 2));
    c.eq("bc anchored", find(b"bc", range(0, 8), CFDataSearchFlags::Anchored), (-1, 0));
    c.eq("ab anchored", find(b"ab", range(0, 8), CFDataSearchFlags::Anchored), (0, 2));
    c.eq(
        "ab anchored backwards",
        find(b"ab", range(0, 8), CFDataSearchFlags::Anchored | CFDataSearchFlags::Backwards),
        (6, 2),
    );
    c.eq(
        "abc anchored backwards",
        find(b"abc", range(0, 8), CFDataSearchFlags::Anchored | CFDataSearchFlags::Backwards),
        (-1, 0),
    );
    c.eq("missing", find(b"x", range(0, 8), none), (-1, 0));
    c.eq("outside the range", find(b"abc", range(4, 4), none), (-1, 0));
    c.eq("too long", find(b"abcabcabc", range(0, 8), none), (-1, 0));
    c.done();
}

#[test]
fn errors_from_keys_and_values() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let domain = CFString::from_str("org.sidestep.test");
    let (k1, k2) = (CFString::from_str("NSLocalizedDescription"), CFString::from_str("extra"));
    let (v1, v2) = (CFString::from_str("It broke"), num(7));
    let keys = [(&*k1 as *const CFString).cast::<c_void>(), (&*k2 as *const CFString).cast::<c_void>()];
    let values = [(&*v1 as *const CFString).cast::<c_void>(), (&*v2 as *const CFNumber).cast::<c_void>()];
    let error =
        unsafe { CFErrorCreateWithUserInfoKeysAndValues(None, Some(&domain), 42, keys.as_ptr(), values.as_ptr(), 2) };
    let error: CFRetained<CFError> = error.unwrap();
    c.eq("code", error.code(), 42);
    c.eq("domain", error.domain().map(|d| d.to_string()), Some("org.sidestep.test".into()));
    c.eq("description", error.description().map(|d| d.to_string()), Some("It broke".into()));
    let info = error.user_info().unwrap();
    c.eq("user info count", info.count(), 2);
    let empty = unsafe {
        CFErrorCreateWithUserInfoKeysAndValues(None, Some(&domain), 1, std::ptr::null(), std::ptr::null(), 0)
    };
    let empty: CFRetained<CFError> = empty.unwrap();
    c.eq("empty user info", empty.user_info().map(|i| i.count()), Some(0));
    c.done();
}

#[test]
fn number_constants() {
    let mut c = Checks::default();
    let value = |n: Option<&CFNumber>| n.and_then(|n| n.as_f64());
    let nan = value(unsafe { objc2_core_foundation::kCFNumberNaN });
    c.eq("NaN", nan.is_some_and(f64::is_nan), true);
    c.eq("+inf", value(unsafe { objc2_core_foundation::kCFNumberPositiveInfinity }), Some(f64::INFINITY));
    c.eq("-inf", value(unsafe { objc2_core_foundation::kCFNumberNegativeInfinity }), Some(f64::NEG_INFINITY));
    let n = unsafe { objc2_core_foundation::kCFNumberNaN }.unwrap();
    c.eq("NaN is a float", objc2_core_foundation::CFNumberIsFloatType(n), true);
    c.done();
}

unsafe extern "C-unwind" fn never(_timer: *mut objc2_core_foundation::CFRunLoopTimer, _info: *mut c_void) {}

#[test]
fn a_run_loops_modes() {
    // A new thread's run loop knows the default mode; a mode that a timer
    // was added in, or that it ran in, joins it. Running in an empty mode
    // returns at once and adds nothing.
    let modes = std::thread::spawn(|| {
        use objc2_core_foundation::*;
        let run_loop = CFRunLoop::current().unwrap();
        let names = |run_loop: &CFRunLoop| {
            let modes = CFRunLoopCopyAllModes(run_loop).unwrap();
            let n = CFArrayGetCount(&modes);
            let mut names: Vec<String> = (0..n).map(|i| text(unsafe { CFArrayGetValueAtIndex(&modes, i) })).collect();
            names.sort();
            names
        };
        let fresh = names(&run_loop);
        let empty = CFString::from_str("org.sidestep.cf-empty");
        let _ = CFRunLoopRunInMode(Some(&empty), 0.0, true);
        let after_empty = names(&run_loop);
        let mode = CFString::from_str("org.sidestep.cf-mode");
        let timer = unsafe {
            CFRunLoopTimerCreate(None, CFAbsoluteTimeGetCurrent() + 1e6, 0.0, 0, 0, Some(never), std::ptr::null_mut())
        }
        .unwrap();
        CFRunLoopAddTimer(&run_loop, Some(&timer), Some(&mode));
        let with_timer = names(&run_loop);
        CFRunLoopTimerInvalidate(&timer);
        (fresh, after_empty, with_timer)
    })
    .join()
    .unwrap();
    let default = unsafe { kCFRunLoopDefaultMode }.unwrap().to_string();
    assert_eq!(modes.0, vec![default.clone()]);
    assert_eq!(modes.1, vec![default.clone()]);
    assert_eq!(modes.2, vec![default, "org.sidestep.cf-mode".to_string()]);
}

#[test]
fn type_ids_differ() {
    use objc2_core_foundation::*;
    let ids = [CFArray::type_id(), CFSet::type_id(), CFDictionary::type_id(), CFString::type_id()];
    for (i, a) in ids.iter().enumerate() {
        for b in &ids[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

mod value_arrays {
    //! Arrays made with other callbacks than `kCFTypeArrayCallBacks`, or
    //! none: their values needn't be objects.
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use objc2_core_foundation::*;

    use super::{Checks, range};

    static RETAINS: AtomicIsize = AtomicIsize::new(0);
    static RELEASES: AtomicIsize = AtomicIsize::new(0);

    unsafe extern "C-unwind" fn counted_retain(_: *const CFAllocator, v: *const c_void) -> *const c_void {
        RETAINS.fetch_add(1, Ordering::SeqCst);
        v
    }

    unsafe extern "C-unwind" fn counted_release(_: *const CFAllocator, _: *const c_void) {
        RELEASES.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C-unwind" fn described(v: *const c_void) -> *const CFString {
        CFRetained::into_raw(CFString::from_str(&format!("<v{}>", v as usize))).as_ptr()
    }

    unsafe extern "C-unwind" fn equal_last_digit(a: *const c_void, b: *const c_void) -> u8 {
        u8::from(a as usize % 10 == b as usize % 10)
    }

    unsafe extern "C-unwind" fn by_address(a: *const c_void, b: *const c_void, _: *mut c_void) -> CFComparisonResult {
        CFComparisonResult((a as usize).cmp(&(b as usize)) as CFIndex)
    }

    /// Retains and releases since the last call.
    fn counts() -> (isize, isize) {
        (RETAINS.swap(0, Ordering::SeqCst), RELEASES.swap(0, Ordering::SeqCst))
    }

    fn values(a: &CFArray) -> Vec<usize> {
        (0..CFArrayGetCount(a)).map(|i| unsafe { CFArrayGetValueAtIndex(a, i) } as usize).collect()
    }

    fn make(values: &[usize], callbacks: *const CFArrayCallBacks) -> CFRetained<CFArray> {
        let mut values: Vec<*const c_void> = values.iter().map(|&v| v as *const c_void).collect();
        unsafe { CFArrayCreate(None, values.as_mut_ptr(), values.len() as CFIndex, callbacks) }.unwrap()
    }

    /// The description with the array's and allocator's addresses left out.
    fn described_as(a: &CFArray) -> Option<String> {
        CFCopyDescription(Some(a)).unwrap().to_string().split_once("]>").map(|(_, rest)| rest.to_string())
    }

    #[test]
    fn without_callbacks() {
        let mut c = Checks::default();
        let a = make(&[3, 1, 2, 1], std::ptr::null());
        c.eq("type", CFGetTypeID(Some(&a)), CFArray::type_id());
        c.eq("values", values(&a), vec![3, 1, 2, 1]);
        c.eq(
            "description",
            described_as(&a),
            Some(
                "{type = immutable, count = 4, values = (\n\t0 : <0x3>\n\t1 : <0x1>\n\t2 : <0x2>\n\t3 : <0x1>\n)}"
                    .into(),
            ),
        );
        unsafe {
            c.eq("count of 1", CFArrayGetCountOfValue(&a, range(0, 4), 1 as _), 2);
            c.eq("first 1", CFArrayGetFirstIndexOfValue(&a, range(0, 4), 1 as _), 1);
            c.eq("last 1", CFArrayGetLastIndexOfValue(&a, range(0, 4), 1 as _), 3);
        }
        c.eq("equal to the same values", CFEqual(Some(&a), Some(&make(&[3, 1, 2, 1], std::ptr::null()))), true);
        c.eq("hash", CFHash(Some(&a)), 4);
        let m = unsafe { CFArrayCreateMutable(None, 0, std::ptr::null()) }.unwrap();
        for v in [5usize, 4, 9] {
            unsafe { CFArrayAppendValue(Some(&m), v as _) };
        }
        unsafe { CFArrayInsertValueAtIndex(Some(&m), 1, 7 as _) };
        unsafe { CFArraySetValueAtIndex(Some(&m), 0, 8 as _) };
        c.eq("changed", values(&m), vec![8, 7, 4, 9]);
        unsafe { CFArraySetValueAtIndex(Some(&m), 4, 6 as _) };
        c.eq("set one past the end", values(&m), vec![8, 7, 4, 9, 6]);
        unsafe { CFArrayRemoveValueAtIndex(Some(&m), 4) };
        unsafe { CFArraySortValues(Some(&m), range(0, 4), Some(by_address), std::ptr::null_mut()) };
        c.eq("sorted", values(&m), vec![4, 7, 8, 9]);
        c.eq(
            "binary search",
            unsafe { CFArrayBSearchValues(&m, range(0, 4), 7 as _, Some(by_address), std::ptr::null_mut()) },
            1,
        );
        unsafe { CFArrayAppendArray(Some(&m), Some(&a), range(1, 2)) };
        c.eq("appended an array", values(&m), vec![4, 7, 8, 9, 1, 2]);
        let mut new = [11usize as *const c_void, 12 as _];
        unsafe { CFArrayReplaceValues(Some(&m), range(0, 3), new.as_mut_ptr(), 2) };
        c.eq("replaced", values(&m), vec![11, 12, 9, 1, 2]);
        let copy = unsafe { CFArrayCreateCopy(None, Some(&m)) }.unwrap();
        c.eq("copy", values(&copy), vec![11, 12, 9, 1, 2]);
        c.eq("copy equal", CFEqual(Some(&m), Some(&copy)), true);
        c.eq(
            "mutable copy",
            values(&unsafe { CFArrayCreateMutableCopy(None, 0, Some(&a)) }.unwrap()),
            vec![3, 1, 2, 1],
        );
        let empty = make(&[], std::ptr::null());
        c.eq("empty description", described_as(&empty), Some("{type = immutable, count = 0, values = ()}".into()));
        let no_objects = CFArray::<CFString>::from_CFTypes(&[]);
        c.eq("empty equals an empty array of objects", CFEqual(Some(&empty), Some(no_objects.as_opaque())), true);
        c.done();
    }

    #[test]
    fn with_callbacks() {
        let mut c = Checks::default();
        let callbacks = CFArrayCallBacks {
            version: 0,
            retain: Some(counted_retain),
            release: Some(counted_release),
            copyDescription: Some(described),
            equal: Some(equal_last_digit),
        };
        counts();
        let a = make(&[1, 12, 23], &callbacks);
        c.eq("made: retained", counts(), (3, 0));
        c.eq(
            "description",
            described_as(&a),
            Some("{type = immutable, count = 3, values = (\n\t0 : <v1>\n\t1 : <v12>\n\t2 : <v23>\n)}".into()),
        );
        c.eq("count of an equal value", unsafe { CFArrayGetCountOfValue(&a, range(0, 3), 33 as _) }, 1);
        let copy = unsafe { CFArrayCreateCopy(None, Some(&a)) }.unwrap();
        c.eq("copy: another array", std::ptr::eq(&*copy, &*a), false);
        c.eq("copy: retained", counts(), (3, 0));
        let m = unsafe { CFArrayCreateMutableCopy(None, 0, Some(&a)) }.unwrap();
        c.eq("mutable copy: retained", counts(), (3, 0));
        c.eq(
            "mutable description",
            described_as(&m).map(|d| d.split(", count").next().unwrap().to_string()),
            Some("{type = mutable-small".into()),
        );
        unsafe { CFArraySetValueAtIndex(Some(&m), 0, 4 as _) };
        c.eq("set", counts(), (1, 1));
        unsafe { CFArrayExchangeValuesAtIndices(Some(&m), 0, 2) };
        c.eq("exchange", counts(), (0, 0));
        unsafe { CFArraySortValues(Some(&m), range(0, 3), Some(by_address), std::ptr::null_mut()) };
        c.eq("sort", (counts(), values(&m)), ((3, 3), vec![4, 12, 23]));
        unsafe { CFArrayRemoveValueAtIndex(Some(&m), 0) };
        c.eq("remove", counts(), (0, 1));
        c.eq("unequal counts", CFEqual(Some(&a), Some(&m)), false);
        let same = unsafe { CFArrayCreateMutableCopy(None, 0, Some(&a)) }.unwrap();
        c.eq("equal copies", CFEqual(Some(&a), Some(&same)), true);
        c.eq("equal by the callback", CFEqual(Some(&a), Some(&make(&[11, 2, 3], &callbacks))), true);
        let plain = make(&[1, 12, 23], std::ptr::null());
        c.eq(
            "other callbacks: unequal",
            (CFEqual(Some(&a), Some(&plain)), CFEqual(Some(&plain), Some(&a))),
            (false, false),
        );
        let bare = CFArrayCallBacks { version: 0, retain: None, release: None, copyDescription: None, equal: None };
        c.eq("no equal callback either way: equal", CFEqual(Some(&make(&[1, 12, 23], &bare)), Some(&plain)), true);
        counts();
        drop(same);
        c.eq("released with the array", counts(), (0, 3));
        drop((copy, a));
        counts();
        CFArrayRemoveAllValues(Some(&m));
        c.eq("remove all", counts(), (0, 2));
        c.done();
    }
}
