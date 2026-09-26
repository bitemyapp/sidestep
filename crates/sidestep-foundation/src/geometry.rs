//! Foundation's C functions on ranges, points, sizes and rectangles.
//!
//! objc2-foundation declares these as `extern "C-unwind"` functions taking
//! the structs by value, so they are exported here under their C names.
//! (`NSMakeRange`, `NSMaxRange` and the other inline helpers are Rust code
//! in objc2 and need no symbol.) Edge cases follow what the conformance
//! tests observe on macOS: an empty rectangle is one without positive width
//! and height, intersections that come out empty are the zero rectangle,
//! and the string forms print numbers the way C's `%.17g` does.

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSAlignmentOptions, NSEdgeInsets, NSPoint, NSRange, NSRect, NSRectEdge, NSSize, NSString};

use crate::string::with_str;

const ZERO_RECT: NSRect = NSRect { origin: NSPoint { x: 0.0, y: 0.0 }, size: NSSize { width: 0.0, height: 0.0 } };

// Ranges.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSUnionRange(a: NSRange, b: NSRange) -> NSRange {
    let start = a.location.min(b.location);
    let end = a.end().max(b.end());
    NSRange::new(start, end - start)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSIntersectionRange(a: NSRange, b: NSRange) -> NSRange {
    let start = a.location.max(b.location);
    let end = a.end().min(b.end());
    // Ranges that only touch, or miss each other, intersect in {0, 0}; an
    // empty range inside the other keeps its place.
    if end < start || (end == start && a.length != 0 && b.length != 0) {
        return NSRange::new(0, 0);
    }
    NSRange::new(start, end - start)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSStringFromRange(range: NSRange) -> *mut NSString {
    autoreleased(format!("{{{}, {}}}", range.location, range.length))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSRangeFromString(string: &NSString) -> NSRange {
    // The first two runs of digits, whatever separates them.
    let [location, length] = with_str(string, |s| {
        let mut found = [0usize; 2];
        let mut runs = s.split(|c: char| !c.is_ascii_digit()).filter(|run| !run.is_empty());
        for slot in &mut found {
            match runs.next() {
                Some(run) => *slot = run.parse().unwrap_or(usize::MAX),
                None => break,
            }
        }
        found
    });
    NSRange::new(location, length)
}

// Points, sizes and rectangles.

fn is_empty(r: NSRect) -> bool {
    !(r.size.width > 0.0 && r.size.height > 0.0)
}

fn max_x(r: NSRect) -> f64 {
    r.origin.x + r.size.width
}

fn max_y(r: NSRect) -> f64 {
    r.origin.y + r.size.height
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect { origin: NSPoint { x, y }, size: NSSize { width, height } }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSEqualPoints(a: NSPoint, b: NSPoint) -> Bool {
    Bool::new(a.x == b.x && a.y == b.y)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSEqualSizes(a: NSSize, b: NSSize) -> Bool {
    Bool::new(a.width == b.width && a.height == b.height)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSEqualRects(a: NSRect, b: NSRect) -> Bool {
    Bool::new(
        a.origin.x == b.origin.x
            && a.origin.y == b.origin.y
            && a.size.width == b.size.width
            && a.size.height == b.size.height,
    )
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSIsEmptyRect(r: NSRect) -> Bool {
    Bool::new(is_empty(r))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSEdgeInsetsEqual(a: NSEdgeInsets, b: NSEdgeInsets) -> Bool {
    Bool::new(a.top == b.top && a.left == b.left && a.bottom == b.bottom && a.right == b.right)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSInsetRect(r: NSRect, dx: f64, dy: f64) -> NSRect {
    rect(r.origin.x + dx, r.origin.y + dy, r.size.width - 2.0 * dx, r.size.height - 2.0 * dy)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSOffsetRect(r: NSRect, dx: f64, dy: f64) -> NSRect {
    rect(r.origin.x + dx, r.origin.y + dy, r.size.width, r.size.height)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSIntegralRect(r: NSRect) -> NSRect {
    if is_empty(r) {
        return ZERO_RECT;
    }
    let (x, y) = (r.origin.x.floor(), r.origin.y.floor());
    rect(x, y, max_x(r).ceil() - x, max_y(r).ceil() - y)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSIntegralRectWithOptions(r: NSRect, options: NSAlignmentOptions) -> NSRect {
    let o = options.0;
    let flipped = options.contains(NSAlignmentOptions::AlignRectFlipped);
    // Each edge or length rounds inward, outward or to the nearest whole
    // number; for each axis two of the three are given and the third
    // follows from them.
    let axis = |min: f64, len: f64, shift: u32, flip: bool| -> (f64, f64) {
        let max = min + len;
        let nearest = |v: f64| if flip { (v - 0.5).ceil() } else { (v + 0.5).floor() };
        let pick = |v: f64, bit: u32, inward: fn(f64) -> f64, outward: fn(f64) -> f64| -> Option<f64> {
            if o & (1 << bit) != 0 {
                Some(inward(v))
            } else if o & (1 << (bit + 8)) != 0 {
                Some(outward(v))
            } else if o & (1 << (bit + 16)) != 0 {
                Some(nearest(v))
            } else {
                None
            }
        };
        let new_min = pick(min, shift, f64::ceil, f64::floor);
        let new_max = pick(max, shift + 2, f64::floor, f64::ceil);
        let new_len = pick(len, shift + 4, f64::floor, f64::ceil);
        match (new_min, new_max, new_len) {
            (Some(a), Some(b), _) => (a, b - a),
            (Some(a), None, Some(l)) => (a, l),
            (None, Some(b), Some(l)) => (b - l, l),
            (Some(a), None, None) => (a, len),
            (None, Some(b), None) => (b - len, len),
            (None, None, Some(l)) => (min, l),
            (None, None, None) => (min, len),
        }
    };
    let (x, width) = axis(r.origin.x, r.size.width, 0, false);
    let (y, height) = axis(r.origin.y, r.size.height, 1, flipped);
    rect(x, y, width, height)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSUnionRect(a: NSRect, b: NSRect) -> NSRect {
    match (is_empty(a), is_empty(b)) {
        (true, true) => ZERO_RECT,
        (true, false) => b,
        (false, true) => a,
        (false, false) => {
            let (x, y) = (a.origin.x.min(b.origin.x), a.origin.y.min(b.origin.y));
            rect(x, y, max_x(a).max(max_x(b)) - x, max_y(a).max(max_y(b)) - y)
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSIntersectionRect(a: NSRect, b: NSRect) -> NSRect {
    let (x, y) = (a.origin.x.max(b.origin.x), a.origin.y.max(b.origin.y));
    let (right, top) = (max_x(a).min(max_x(b)), max_y(a).min(max_y(b)));
    if right <= x || top <= y {
        return ZERO_RECT;
    }
    rect(x, y, right - x, top - y)
}

/// # Safety
/// `slice` and `rem` must be valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSDivideRect(
    r: NSRect,
    slice: NonNull<NSRect>,
    rem: NonNull<NSRect>,
    amount: f64,
    edge: NSRectEdge,
) {
    let (x, y, w, h) = (r.origin.x, r.origin.y, r.size.width, r.size.height);
    let (s, remaining) = match edge {
        NSRectEdge::MinX => {
            let a = amount.min(w);
            (rect(x, y, a, h), rect(x + a, y, w - a, h))
        }
        NSRectEdge::MaxX => {
            let a = amount.min(w);
            (rect(x + w - a, y, a, h), rect(x, y, w - a, h))
        }
        NSRectEdge::MinY => {
            let a = amount.min(h);
            (rect(x, y, w, a), rect(x, y + a, w, h - a))
        }
        _ => {
            let a = amount.min(h);
            (rect(x, y + h - a, w, a), rect(x, y, w, h - a))
        }
    };
    // SAFETY: guaranteed by the caller.
    unsafe {
        slice.write(s);
        rem.write(remaining);
    }
}

/// Whether `p` is in `r`, counting the edges nearest the origin in a
/// flipped rectangle and the opposite ones otherwise, as mouse hits do.
fn mouse_in(p: NSPoint, r: NSRect, flipped: bool) -> bool {
    if is_empty(r) || p.x < r.origin.x || p.x >= max_x(r) {
        return false;
    }
    if flipped { p.y >= r.origin.y && p.y < max_y(r) } else { p.y > r.origin.y && p.y <= max_y(r) }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSPointInRect(p: NSPoint, r: NSRect) -> Bool {
    Bool::new(mouse_in(p, r, true))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSMouseInRect(p: NSPoint, r: NSRect, flipped: Bool) -> Bool {
    Bool::new(mouse_in(p, r, flipped.as_bool()))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSContainsRect(a: NSRect, b: NSRect) -> Bool {
    Bool::new(
        !is_empty(a)
            && !is_empty(b)
            && a.origin.x <= b.origin.x
            && a.origin.y <= b.origin.y
            && max_x(a) >= max_x(b)
            && max_y(a) >= max_y(b),
    )
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSIntersectsRect(a: NSRect, b: NSRect) -> Bool {
    Bool::new(
        !is_empty(a)
            && !is_empty(b)
            && a.origin.x < max_x(b)
            && b.origin.x < max_x(a)
            && a.origin.y < max_y(b)
            && b.origin.y < max_y(a),
    )
}

// String forms.

/// A number as C's `%.17g` prints it, which is how Foundation writes
/// geometry: `2.5`, `0.10000000000000001`, `1e+20`, `-0`, `nan`.
fn g17(v: f64) -> String {
    let mut buf = [0u8; 40];
    // SAFETY: a bounded snprintf of one double into a local buffer.
    let n = unsafe { libc::snprintf(buf.as_mut_ptr().cast(), buf.len(), c"%.17g".as_ptr(), v) };
    String::from_utf8_lossy(&buf[..n.clamp(0, 39) as usize]).into_owned()
}

fn autoreleased(text: String) -> *mut NSString {
    Retained::autorelease_return(NSString::from_str(&text))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSStringFromPoint(p: NSPoint) -> *mut NSString {
    autoreleased(format!("{{{}, {}}}", g17(p.x), g17(p.y)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSStringFromSize(s: NSSize) -> *mut NSString {
    autoreleased(format!("{{{}, {}}}", g17(s.width), g17(s.height)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSStringFromRect(r: NSRect) -> *mut NSString {
    autoreleased(format!(
        "{{{{{}, {}}}, {{{}, {}}}}}",
        g17(r.origin.x),
        g17(r.origin.y),
        g17(r.size.width),
        g17(r.size.height)
    ))
}

/// The first `N` numbers in a string, skipping anything that doesn't start
/// one; missing numbers are 0.
fn numbers<const N: usize>(string: &NSString) -> [f64; N] {
    with_str(string, |s| {
        let mut out = [0.0; N];
        let mut rest = s;
        for slot in &mut out {
            loop {
                if rest.is_empty() {
                    return out;
                }
                if let Some((v, len)) = float_prefix(rest) {
                    *slot = v;
                    rest = &rest[len..];
                    break;
                }
                let skip = rest.chars().next().map_or(1, char::len_utf8);
                rest = &rest[skip..];
            }
        }
        out
    })
}

/// The longest decimal floating-point number at the start of `s`, and its
/// length in bytes.
pub(crate) fn float_prefix(s: &str) -> Option<(f64, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let digits_start = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let mut digits = i - digits_start;
    if b.get(i) == Some(&b'.') {
        let frac = i + 1;
        let mut j = frac;
        while b.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        digits += j - frac;
        if digits > 0 {
            i = j;
        }
    }
    if digits == 0 {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let exp_start = j;
        while b.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if j > exp_start {
            i = j;
        }
    }
    s[..i].parse().ok().map(|v| (v, i))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSPointFromString(string: &NSString) -> NSPoint {
    let [x, y] = numbers(string);
    NSPoint { x, y }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSSizeFromString(string: &NSString) -> NSSize {
    let [width, height] = numbers(string);
    NSSize { width, height }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSRectFromString(string: &NSString) -> NSRect {
    let [x, y, width, height] = numbers(string);
    rect(x, y, width, height)
}
