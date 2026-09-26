//! String costs, in nanoseconds per operation (median of seven runs). Run in
//! release mode on macOS (Apple's Foundation) and on Linux (Sidestep's) to
//! compare: `cargo run --release -p strbench`.
//!
//! Measured on an M-series Mac: macOS natively, Linux in a VM on the same
//! machine.
//!
//! Runs vary by 10-20% from one to the next, more in the VM.
//!
//! | operation                                         |     Apple |  Sidestep |
//! |---------------------------------------------------|----------:|----------:|
//! | from_str("hello") + release                       |        16 |        17 |
//! | from_str(1 KB ASCII) + release                    |        99 |        72 |
//! | from_str(1 KB mixed) + release                    |     1,665 |       689 |
//! | to_string (1 KB mixed)                            |     2,256 |        21 |
//! | isEqualToString: (1 KB mixed, equal)              |       217 |        12 |
//! | hash (1 KB mixed, fresh string)                   |     1,841 |       877 |
//! | characterAtIndex: sequential (1 MB mixed)         |       3.8 |       4.0 |
//! | characterAtIndex: random (1 MB mixed)             |       4.2 |        37 |
//! | substringWithRange: (40 units, 1 MB mixed)        |        79 |        75 |
//! | rangeOfString: absent (1 MB mixed)                | 6,438,909 |    20,722 |
//! | rangeOfString: absent, non-ASCII (1 MB mixed)     | 5,600,167 | 5,394,421 |
//! | replace every "the" (1 MB mixed, 16k hits)        | 7,489,927 |   532,906 |
//! | replace every "café" (1 MB mixed, 16k hits)       | 6,122,042 | 6,094,169 |
//! | rangeOfString: case-insensitive, absent (1 KB)    |     6,998 |        46 |
//! | caseInsensitiveCompare: (1 KB mixed, equal)       |     4,664 |     4,898 |
//! | lowercaseString (1 KB mixed)                      |     5,519 |     5,271 |
//! | rangeOfString: non-ASCII, early hit (1 MB mixed)  |       153 |       482 |
//! | rangeOfString: case+diacritic, early hit (1 MB)   |        59 |       250 |
//! | NSMutableString replace every "the" (1 MB mixed)  | 7,519,954 |   542,540 |
//! | NSMutableString replace "the" with "a" (1.3 MB)   | 8,436,167 | 1,008,679 |
//! | compare: (1 KB mixed, differ at the start)        |        33 |        10 |
//! | compare: (1 KB mixed, differ at the end)          |     2,058 |       256 |
//! | caseInsensitiveCompare: (1 KB, differ early)      |        39 |        10 |
//! | rangeOfString: regular expression (32-byte line)  |     1,024 |       859 |
//! | NSScanner scanDouble: (loop over 20k numbers)     |       105 |        34 |
//! | NSScanner key=value; (loop over 20k pairs)        |       731 |       144 |
//! | percent-encode (64 KB mixed, query set)           |    80,027 |   106,441 |
//! | enumerateLinesUsingBlock: (own subclass, 16k)     | 4,990,171 | 2,323,272 |
//! | restyle: 20 addAttribute: calls, 10k runs         |    89,293 |   112,099 |
//! | attributesAtIndex:effectiveRange: walk, 10k runs  |       9.3 |      29.8 |
//! | appendAttributedString: (streaming, 100k+)        |        55 |        68 |
//! | addAttribute: whole text, 10k distinct runs       |   693,740 | 1,369,879 |
//! | addAttribute: whole text, 50k distinct runs       | 3,423,942 |11,216,319 |
//! | length (subclass overriding the primitives)       |        21 |        25 |
//! | attribute:atIndex:effectiveRange: (same subclass) |        27 |        53 |
//!
//! The rows from "early hit" down measure work that should cost what it
//! touches: before the review that prompted them, a non-ASCII hit near the
//! start of a megabyte took 4.6 ms, a mutable replace-all 6.1 s, a
//! scanDouble: loop over 80k numbers 94 µs a number, and a whole-text
//! restyle over 50k distinct runs 992 ms.
//!
//! Random `characterAtIndex:` pays for UTF-8 storage: Apple keeps mixed
//! text as UTF-16, Sidestep walks from the nearest index crumb (every 64
//! units). The attributed rows pay for the runtime's autorelease path and
//! for building attribute dictionaries.

use std::hint::black_box;
use std::time::Instant;

use std::cell::Cell;

use block2::RcBlock;
use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{
    NSAttributedString, NSCharacterSet, NSDictionary, NSMutableAttributedString, NSMutableCopying, NSMutableString,
    NSRange, NSScanner, NSString, NSStringCompareOptions,
};

use sidestep as _;

/// Median of seven timed runs of `iters` calls, after a warm-up, in
/// autorelease pools of a thousand calls.
fn bench(name: &str, iters: usize, mut f: impl FnMut(usize)) {
    let mut run = |n: usize| {
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
    run(iters / 10);
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = Instant::now();
            run(iters);
            start.elapsed().as_nanos() as f64 / iters as f64
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    println!("{name:<48} {:>10.2} ns  (min {:.2})", runs[3], runs[0]);
}

/// Mixed text: ASCII words, accented Latin, CJK and emoji, roughly as a
/// chat transcript mixes them.
fn mixed(bytes: usize) -> String {
    let words = ["the ", "quick ", "brown ", "fox ", "jumps ", "grüße ", "café ", "漢字 ", "かな ", "🎉 ", "naïve\n"];
    let mut s = String::new();
    let mut i = 0;
    while s.len() < bytes {
        s.push_str(words[i % words.len()]);
        i += 7;
    }
    s
}

fn main() {
    let hello = "hello";
    let ascii_1k: String = "abcdefghijklmnopqrstuvwxyz0123456789 ".repeat(28)[..1024].to_string();
    let mixed_1k = mixed(1024);
    bench("from_str(\"hello\") + release", 5_000_000, |_| {
        drop(black_box(NSString::from_str(black_box(hello))));
    });
    bench("from_str(1 KB ASCII) + release", 1_000_000, |_| {
        drop(black_box(NSString::from_str(black_box(&ascii_1k))));
    });
    bench("from_str(1 KB mixed) + release", 1_000_000, |_| {
        drop(black_box(NSString::from_str(black_box(&mixed_1k))));
    });

    let s = NSString::from_str(&mixed_1k);
    let same = NSString::from_str(&mixed_1k);
    bench("to_string (1 KB mixed)", 1_000_000, |_| {
        black_box(black_box(&s).to_string());
    });
    bench("isEqualToString: (1 KB mixed, equal)", 5_000_000, |_| {
        black_box(black_box(&s).isEqualToString(&same));
    });
    bench("hash (1 KB mixed, fresh string)", 500_000, |_| {
        let t = NSString::from_str(&mixed_1k);
        black_box(t.hash());
    });

    let big = mixed(1 << 20);
    let big_s = NSString::from_str(&big);
    let len = big_s.length();
    let seq_iters = len;
    bench("characterAtIndex: sequential (1 MB mixed)", seq_iters, |i| {
        black_box(big_s.characterAtIndex(i % len));
    });
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let random: Vec<usize> = (0..1 << 16)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % len as u64) as usize
        })
        .collect();
    bench("characterAtIndex: random (1 MB mixed)", 2_000_000, |i| {
        black_box(big_s.characterAtIndex(random[i & 0xFFFF]));
    });
    bench("substringWithRange: (40 units, 1 MB mixed)", 2_000_000, |i| {
        let at = random[i & 0xFFFF].min(len - 40);
        black_box(big_s.substringWithRange(NSRange::new(at, 40)));
    });
    search(&big_s, &s);
    walks(&big, &big_s, &mixed_1k);
    let _: Retained<NSString> = big_s;

    attributed();
    restyles_and_subclasses();
}

/// Searching and comparing: a whole-text scan for an absent needle, and
/// the folded forms on a kilobyte.
fn search(big: &NSString, small: &NSString) {
    let absent = NSString::from_str("zebra");
    bench("rangeOfString: absent (1 MB mixed)", 200, |_| {
        black_box(big.rangeOfString(&absent));
    });
    let accented = NSString::from_str("grüßen");
    bench("rangeOfString: absent, non-ASCII (1 MB mixed)", 20, |_| {
        black_box(big.rangeOfString(&accented));
    });
    let (the, cafe) = (NSString::from_str("the"), NSString::from_str("café"));
    let (the_up, cafe_up) = (NSString::from_str("THE"), NSString::from_str("CAFE"));
    bench("replace every \"the\" (1 MB mixed, 16k hits)", 20, |_| {
        black_box(big.stringByReplacingOccurrencesOfString_withString(&the, &the_up));
    });
    bench("replace every \"café\" (1 MB mixed, 16k hits)", 20, |_| {
        black_box(big.stringByReplacingOccurrencesOfString_withString(&cafe, &cafe_up));
    });
    let late = NSString::from_str("ZEBRA");
    let ci = NSStringCompareOptions::CaseInsensitiveSearch;
    let all = NSRange::new(0, small.length());
    bench("rangeOfString: case-insensitive, absent (1 KB mixed)", 100_000, |_| {
        black_box(small.rangeOfString_options_range(&late, ci, all));
    });
    let upper = small.uppercaseString();
    bench("caseInsensitiveCompare: (1 KB mixed, equal)", 100_000, |_| {
        black_box(small.caseInsensitiveCompare(&upper));
    });
    bench("lowercaseString (1 KB mixed)", 100_000, |_| {
        black_box(upper.lowercaseString());
    });
}

/// Work that should cost what it touches: an early hit, a first
/// difference, one scanned number, one line.
fn walks(big: &str, big_s: &NSString, mixed_1k: &str) {
    let (cafe, the) = (NSString::from_str("café"), NSString::from_str("the"));
    let all = NSRange::new(0, big_s.length());
    bench("rangeOfString: non-ASCII, early hit (1 MB mixed)", 100_000, |_| {
        black_box(big_s.rangeOfString(&cafe));
    });
    let folding = NSStringCompareOptions::CaseInsensitiveSearch | NSStringCompareOptions::DiacriticInsensitiveSearch;
    bench("rangeOfString: case+diacritic, early hit (1 MB)", 100_000, |_| {
        black_box(big_s.rangeOfString_options_range(&the, folding, all));
    });
    let the_up = NSString::from_str("THE");
    bench("NSMutableString replace every \"the\" (1 MB mixed)", 10, |_| {
        let m: Retained<NSMutableString> = big_s.mutableCopy();
        black_box(m.replaceOccurrencesOfString_withString_options_range(&the, &the_up, NSStringCompareOptions(0), all));
    });
    let ascii = NSString::from_str(&"the quick brown fox ".repeat(1 << 16));
    let a = NSString::from_str("a");
    bench("NSMutableString replace \"the\" with \"a\" (1.3 MB ASCII)", 10, |_| {
        let m: Retained<NSMutableString> = ascii.mutableCopy();
        let whole = NSRange::new(0, m.length());
        black_box(m.replaceOccurrencesOfString_withString_options_range(&the, &a, NSStringCompareOptions(0), whole));
    });
    let (x, y) = (NSString::from_str(&format!("a{mixed_1k}")), NSString::from_str(&format!("b{mixed_1k}")));
    bench("compare: (1 KB mixed, differ at the start)", 1_000_000, |_| {
        black_box(x.compare(&y));
    });
    let (x2, y2) = (NSString::from_str(&format!("{mixed_1k}a")), NSString::from_str(&format!("{mixed_1k}b")));
    bench("compare: (1 KB mixed, differ at the end)", 1_000_000, |_| {
        black_box(x2.compare(&y2));
    });
    bench("caseInsensitiveCompare: (1 KB mixed, differ early)", 1_000_000, |_| {
        black_box(x.caseInsensitiveCompare(&y));
    });
    let regex = NSString::from_str("[ \t]+$");
    let line = NSString::from_str("    let x = compute(y);  \t");
    let line_all = NSRange::new(0, line.length());
    bench("rangeOfString: regular expression (32-byte line)", 200_000, |_| {
        black_box(line.rangeOfString_options_range(&regex, NSStringCompareOptions::RegularExpressionSearch, line_all));
    });
    let numbers = NSString::from_str(&(0..20_000).map(|i| format!("{}.{} ", i, i % 97)).collect::<String>());
    let scanner = NSScanner::scannerWithString(&numbers);
    bench("NSScanner scanDouble: (loop over 20k numbers)", 1_000_000, |_| {
        let mut d = 0.0;
        if !unsafe { scanner.scanDouble(&mut d) } {
            scanner.setScanLocation(0);
        }
        black_box(d);
    });
    let pairs = NSString::from_str(&(0..20_000).map(|i| format!("key{i}=value{i};")).collect::<String>());
    let scanner = NSScanner::scannerWithString(&pairs);
    let (eq, semi) = (NSString::from_str("="), NSString::from_str(";"));
    bench("NSScanner key=value; (loop over 20k pairs)", 500_000, |_| {
        if scanner.isAtEnd() {
            scanner.setScanLocation(0);
        }
        let mut out: Option<Retained<NSString>> = None;
        scanner.scanUpToString_intoString(&eq, Some(&mut out));
        scanner.scanString_intoString(&eq, None);
        scanner.scanUpToString_intoString(&semi, Some(&mut out));
        scanner.scanString_intoString(&semi, None);
        black_box(out);
    });
    let query = NSCharacterSet::URLQueryAllowedCharacterSet();
    let text_64k = NSString::from_str(&big[..big.floor_char_boundary(1 << 16)]);
    bench("percent-encode (64 KB mixed, query set)", 200, |_| {
        black_box(text_64k.stringByAddingPercentEncodingWithAllowedCharacters(&query));
    });
    let lines: String = (0..16_000).map(|i| format!("line {i} of text\n")).collect();
    let foreign = Units::string(&lines);
    let count = std::rc::Rc::new(Cell::new(0));
    let counter = count.clone();
    let block = RcBlock::new(move |line: std::ptr::NonNull<NSString>, _stop: std::ptr::NonNull<Bool>| {
        black_box(line);
        counter.set(counter.get() + 1);
    });
    bench("enumerateLinesUsingBlock: (own subclass, 16k lines)", 10, |_| {
        foreign.enumerateLinesUsingBlock(&block);
    });
    assert!(count.get() > 0);
}

/// A string class of the app's own, holding UTF-16 and implementing only
/// the two primitives.
struct UnitsIvars {
    units: Vec<u16>,
}

define_class!(
    #[unsafe(super(NSString))]
    #[ivars = UnitsIvars]
    struct Units;

    impl Units {
        #[unsafe(method(length))]
        fn length(&self) -> usize {
            self.ivars().units.len()
        }

        #[unsafe(method(characterAtIndex:))]
        fn character_at_index(&self, i: usize) -> u16 {
            self.ivars().units[i]
        }
    }
);

impl Units {
    fn string(text: &str) -> Retained<NSString> {
        let this: Allocated<Self> = Self::alloc();
        let this = this.set_ivars(UnitsIvars { units: text.encode_utf16().collect() });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        Retained::into_super(this)
    }
}

/// A dictionary with one attribute, a fresh object each time.
fn attrs(key: &NSString, value: &NSString) -> Retained<NSDictionary<NSString, AnyObject>> {
    NSDictionary::from_slices(&[key], &[value.as_ref()])
}

fn attributed() {
    // 10,000 words, each its own run.
    let words = 10_000;
    let m = NSMutableAttributedString::from_nsstring(&NSString::from_str(&"word ".repeat(words)));
    let key = NSString::from_str("color");
    let values: Vec<Retained<NSString>> = (0..8).map(|i| NSString::from_str(&format!("value-number-{i}"))).collect();
    autoreleasepool(|_| {
        for w in 0..words {
            unsafe { m.setAttributes_range(Some(&attrs(&key, &values[w % 8])), NSRange::new(w * 5, 5)) };
        }
    });
    let font = NSString::from_str("font");
    bench("restyle: 20 addAttribute: calls, 10k runs", 2_000, |i| {
        for k in 0..20 {
            let at = ((i * 20 + k) * 7919) % (words - 60);
            unsafe { m.addAttribute_value_range(&font, &values[k % 8], NSRange::new(at * 5, 250)) };
        }
    });
    bench("attributesAtIndex:effectiveRange: walk, 10k runs", 1_000_000, |i| {
        let mut r = NSRange::new(0, 0);
        black_box(unsafe { m.attributesAtIndex_effectiveRange((i * 5) % (words * 5), &mut r) });
    });
    let piece = unsafe {
        NSAttributedString::new_with_attributes(&NSString::from_str("streamed text "), &attrs(&key, &values[1]))
    };
    let stream = NSMutableAttributedString::new();
    let mut n = 0;
    bench("appendAttributedString: (streaming, 100k+)", 100_000, |_| {
        stream.appendAttributedString(&piece);
        n += 1;
    });
    assert_eq!(stream.length(), n * 14);
}

/// Restyling text whose every run has a dictionary of its own, and reading
/// through a subclass that overrides the primitives, as a text storage does.
fn restyles_and_subclasses() {
    let key = NSString::from_str("color");
    let font = NSString::from_str("font");
    let values: Vec<Retained<NSString>> = (0..8).map(|i| NSString::from_str(&format!("value-number-{i}"))).collect();
    for words in [10_000, 50_000] {
        let m = NSMutableAttributedString::from_nsstring(&NSString::from_str(&"word ".repeat(words)));
        autoreleasepool(|_| {
            for w in 0..words {
                let own = NSString::from_str(&format!("value-{w}"));
                unsafe { m.setAttributes_range(Some(&attrs(&key, &own)), NSRange::new(w * 5, 5)) };
            }
        });
        let all = NSRange::new(0, m.length());
        let name = format!("addAttribute: whole text, {}k distinct runs", words / 1000);
        bench(&name, 20, |i| unsafe { m.addAttribute_value_range(&font, &values[i % 8], all) });
    }
    let inner = NSMutableAttributedString::from_nsstring(&NSString::from_str(&"word ".repeat(1000)));
    unsafe { inner.addAttribute_value_range(&key, &values[0], NSRange::new(0, 2500)) };
    let storage = Storage::over(&inner);
    bench("length (subclass overriding the primitives)", 2_000_000, |_| {
        black_box(storage.length());
    });
    bench("attribute:atIndex:effectiveRange: (same subclass)", 1_000_000, |i| {
        let mut r = NSRange::new(0, 0);
        black_box(unsafe { storage.attribute_atIndex_effectiveRange(&key, i % 5000, &mut r) });
    });
}

struct StorageIvars {
    inner: Retained<NSMutableAttributedString>,
}

define_class!(
    // Overrides the four primitives, forwarding to an attributed string it
    // holds, as an NSTextStorage keeps its own storage.
    #[unsafe(super(NSMutableAttributedString, NSAttributedString))]
    #[ivars = StorageIvars]
    struct Storage;

    impl Storage {
        #[unsafe(method_id(string))]
        fn string(&self) -> Retained<NSString> {
            self.ivars().inner.string()
        }

        #[unsafe(method_id(attributesAtIndex:effectiveRange:))]
        fn attributes_at(&self, index: usize, range: *mut NSRange) -> Retained<NSDictionary<NSString, AnyObject>> {
            unsafe { self.ivars().inner.attributesAtIndex_effectiveRange(index, range) }
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace(&self, range: NSRange, string: &NSString) {
            self.ivars().inner.replaceCharactersInRange_withString(range, string);
        }

        #[unsafe(method(setAttributes:range:))]
        fn set_attributes(&self, attrs: Option<&NSDictionary<NSString, AnyObject>>, range: NSRange) {
            unsafe { self.ivars().inner.setAttributes_range(attrs, range) };
        }
    }
);

impl Storage {
    fn over(inner: &NSMutableAttributedString) -> Retained<NSMutableAttributedString> {
        let this: Allocated<Self> = Self::alloc();
        let this = this.set_ivars(StorageIvars { inner: inner.mutableCopy() });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        Retained::into_super(this)
    }
}
