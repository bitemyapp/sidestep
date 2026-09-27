//! `NSValue`: a copy of any C value, with its Objective-C type encoding.
//!
//! The bytes are copied in on creation, sized from the encoding with C's
//! layout rules, and copied out by `-getValue:`. `NSNumber` is a subclass
//! that keeps its value in its own form and answers the same messages, so
//! a value and a number holding the same bytes and type compare equal from
//! the value's side, as in Foundation. Values are compared where they lie,
//! without copying; a subclass defined elsewhere is read through the two
//! methods Foundation asks it to implement, `-objCType` and `-getValue:`.

use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::NonNull;
use std::sync::LazyLock;

use objc2::encode::Encode;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSEdgeInsets, NSPoint, NSRange, NSRect, NSSize, NSString, NSUInteger, NSValue, NSZone};

use crate::number::{fast_value, format_g};
use crate::string::hash_bytes;
use crate::util::{self, is_exactly};

/// The encodings of the types Foundation names, as objc2 writes them, which
/// is what `NSValue::contains_encoding` compares with. objc2 names the
/// geometry structs `_NSPoint`, `_NSSize` and `_NSRect` for GNUstep, where
/// Apple's are `CGPoint` and so on; values with either are recognized.
static POINT: LazyLock<CString> = LazyLock::new(encoding::<NSPoint>);
static SIZE: LazyLock<CString> = LazyLock::new(encoding::<NSSize>);
static RECT: LazyLock<CString> = LazyLock::new(encoding::<NSRect>);
static RANGE: LazyLock<CString> = LazyLock::new(encoding::<NSRange>);
static EDGE_INSETS: LazyLock<CString> = LazyLock::new(encoding::<NSEdgeInsets>);
static POINTER: LazyLock<CString> = LazyLock::new(encoding::<*const c_void>);

fn encoding<T: Encode>() -> CString {
    CString::new(T::ENCODING.to_string()).expect("encodings have no NULs")
}

/// Whether `t` is the type objc2 writes as `ours`, or Apple's spelling of it.
fn is_type(t: &CStr, ours: &CStr, apple: &CStr) -> bool {
    t == ours || t == apple
}

#[derive(Default)]
pub(crate) struct ValueIvars {
    bytes: Box<[u8]>,
    objc_type: Box<CStr>,
}

/// Type qualifiers that may prefix a type: const, in, inout, out, bycopy,
/// byref, oneway, atomic, complex.
const QUALIFIERS: &[u8] = b"rnNoORVAj";

/// The size and alignment of the first type in `enc`, by C's layout rules
/// on this platform, and what follows it. `None` for malformed encodings.
pub(crate) fn layout(enc: &[u8]) -> Option<(usize, usize, &[u8])> {
    let skip = enc.iter().take_while(|b| QUALIFIERS.contains(b)).count();
    let (&first, rest) = enc[skip..].split_first()?;
    let scalar = |size: usize| Some((size, size, rest));
    match first {
        b'c' | b'C' | b'B' => scalar(1),
        b's' | b'S' => scalar(2),
        b'i' | b'I' | b'f' => scalar(4),
        b'l' | b'L' => scalar(size_of::<std::ffi::c_long>()),
        b'q' | b'Q' | b'd' => scalar(8),
        b'D' => scalar(16),
        b'v' => Some((0, 1, rest)),
        b'*' | b'#' | b':' | b'?' => scalar(size_of::<usize>()),
        b'@' => {
            // `@"Class"` names a class; `@?` is a block, maybe with `<...>`.
            let rest = match rest {
                [b'"', tail @ ..] => &tail[tail.iter().position(|&b| b == b'"')? + 1..],
                [b'?', b'<', ..] => &rest[bracketed(&rest[1..])? + 1..],
                [b'?', tail @ ..] => tail,
                _ => rest,
            };
            Some((size_of::<usize>(), size_of::<usize>(), rest))
        }
        b'^' => {
            let (_, _, rest) = layout(rest).unwrap_or((0, 1, &rest[rest.len().min(1)..]));
            Some((size_of::<usize>(), size_of::<usize>(), rest))
        }
        b'[' => {
            let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            let count: usize = std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()?;
            let (size, align, rest) = layout(&rest[digits..])?;
            Some((size.checked_mul(count)?, align, rest.strip_prefix(b"]")?))
        }
        b'{' | b'(' => {
            let close = if first == b'{' { b'}' } else { b')' };
            // Skip the name, up to `=`, or to the end for an opaque type.
            let mut fields = rest;
            let name_len = fields.iter().position(|&b| b == b'=' || b == close)?;
            fields = &fields[name_len..];
            let (mut size, mut align) = (0usize, 1usize);
            if let [b'=', tail @ ..] = fields {
                fields = tail;
                while fields.first() != Some(&close) {
                    // Field names may precede each type in quotes.
                    if let [b'"', tail @ ..] = fields {
                        fields = &tail[tail.iter().position(|&b| b == b'"')? + 1..];
                    }
                    let (field_size, field_align, tail) = layout(fields)?;
                    align = align.max(field_align);
                    size = if first == b'{' {
                        size.next_multiple_of(field_align) + field_size
                    } else {
                        size.max(field_size)
                    };
                    fields = tail;
                }
            }
            Some((size.next_multiple_of(align), align, &fields[1..]))
        }
        b'b' => {
            // A bit-field: counted as the bytes its bits need.
            let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
            let bits: usize = std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()?;
            Some((bits.div_ceil(8), 1, &rest[digits..]))
        }
        _ => None,
    }
}

/// The length of a `<...>` group at the start of `s`, less its closing byte.
fn bracketed(s: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in s.iter().enumerate() {
        match b {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The size of a value of type `enc`, or a panic naming the encoding.
fn size_of_type(enc: &CStr) -> usize {
    match layout(enc.to_bytes()) {
        Some((size, _, _)) => size,
        None => panic!("*** -[NSValue initWithBytes:objCType:]: unsupported type encoding {enc:?}"),
    }
}

/// The value of `this` as a `T`, from its first bytes.
fn read<T: Copy>(this: &NSValueImpl) -> T {
    let mut out = std::mem::MaybeUninit::<T>::zeroed();
    with_contents(this, |contents| {
        let bytes = contents.map_or(&[][..], |(_, bytes)| bytes);
        let n = bytes.len().min(size_of::<T>());
        // SAFETY: at most `size_of::<T>()` bytes into a zeroed T; the types
        // read this way (geometry, ranges, pointers) are valid for any bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.as_mut_ptr().cast::<u8>(), n) };
    });
    // SAFETY: as above.
    unsafe { out.assume_init() }
}

/// Whether a freshly allocated object is exactly an NSValue.
fn is_exactly_allocated(this: &Allocated<NSValueImpl>) -> bool {
    let obj = Allocated::as_ptr(this).cast::<AnyObject>();
    // SAFETY: an allocated object is live, with its class set.
    !obj.is_null() && is_exactly(unsafe { &*obj }, &crate::NSVALUE)
}

/// A new value object holding `bytes` of type `objc_type`.
fn make(bytes: &[u8], objc_type: &CStr) -> Retained<NSValueImpl> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSValue::alloc();
    // SAFETY: NSValue's class is NSValueImpl, and an `Allocated` is a
    // pointer to its object whatever its type parameter.
    let this = unsafe { std::mem::transmute::<Allocated<NSValue>, Allocated<NSValueImpl>>(this) };
    init(this, bytes, objc_type)
}

fn init(this: Allocated<NSValueImpl>, bytes: &[u8], objc_type: &CStr) -> Retained<NSValueImpl> {
    let this = this.set_ivars(ValueIvars { bytes: bytes.into(), objc_type: objc_type.into() });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// The bytes of a value of type `objc_type` at `value`.
///
/// # Safety
/// `objc_type` must be a C string and `value` must point to a value of
/// that type; both must outlive the returned references.
unsafe fn raw_parts<'a>(value: NonNull<c_void>, objc_type: NonNull<c_char>) -> (&'a [u8], &'a CStr) {
    // SAFETY: guaranteed by the caller.
    let objc_type = unsafe { CStr::from_ptr(objc_type.as_ptr()) };
    let size = size_of_type(objc_type);
    // SAFETY: as above.
    (unsafe { std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), size) }, objc_type)
}

fn make_raw(value: NonNull<c_void>, objc_type: NonNull<c_char>) -> Retained<NSValueImpl> {
    // SAFETY: the callers of +valueWithBytes:objCType: pass a C string and
    // a value of that type.
    let (bytes, objc_type) = unsafe { raw_parts(value, objc_type) };
    make(bytes, objc_type)
}

fn make_from<T: Copy>(value: &T, objc_type: &CStr) -> Retained<NSValueImpl> {
    // SAFETY: T is a plain C value of `size_of::<T>()` bytes.
    let bytes = unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) };
    make(bytes, objc_type)
}

/// `f` of the type and bytes of any `NSValue`: Sidestep's own values and
/// numbers read in place, subclasses defined elsewhere asked by message.
/// `None` for a subclass whose `-objCType` is NULL or can't be sized.
pub(crate) fn with_contents<R>(value: &AnyObject, f: impl FnOnce(Option<(&CStr, &[u8])>) -> R) -> R {
    if is_exactly(value, &crate::NSVALUE) {
        // SAFETY: an instance of exactly NSValueImpl.
        let ivars = unsafe { &*(value as *const AnyObject).cast::<NSValueImpl>() }.ivars();
        return f(Some((&ivars.objc_type, &ivars.bytes)));
    }
    if let Some(number) = fast_value(value) {
        let mut bytes = [0u8; 8];
        let (objc_type, size) = number.contents(&mut bytes);
        return f(Some((objc_type, &bytes[..size])));
    }
    // SAFETY: -objCType takes nothing and returns a C string that lives as
    // long as the value, or NULL from a broken subclass.
    let objc_type: *const c_char = unsafe { msg_send![value, objCType] };
    if objc_type.is_null() {
        return f(None);
    }
    // SAFETY: a non-null C string, per -objCType.
    let objc_type = unsafe { CStr::from_ptr(objc_type) };
    let Some((size, _, _)) = layout(objc_type.to_bytes()) else { return f(None) };
    // Room to spare, in case the subclass's idea of the size is larger.
    let mut bytes = vec![0u8; size.max(64)];
    let out = NonNull::new(bytes.as_mut_ptr()).expect("non-null").cast::<c_void>();
    // SAFETY: -getValue: fills a buffer with the value, which has room for
    // it (and more).
    let _: () = unsafe { msg_send![value, getValue: out] };
    f(Some((objc_type, &bytes[..size])))
}

/// `-isEqualToValue:`: the same type and the same bytes.
fn values_equal(this: &AnyObject, other: &AnyObject) -> bool {
    std::ptr::eq(this, other)
        || with_contents(this, |mine| with_contents(other, |theirs| mine.is_some() && mine == theirs))
}

/// `-description`: geometry and ranges by name, anything else as bytes.
fn describe(this: &NSValueImpl) -> String {
    let g = |v: f64| format_g(v, 17);
    let objc_type = with_contents(this, |c| c.map(|(t, _)| t.to_owned())).unwrap_or_default();
    let t = &*objc_type;
    if is_type(t, &POINT, c"{CGPoint=dd}") {
        let p: NSPoint = read(this);
        format!("NSPoint: {{{}, {}}}", g(p.x), g(p.y))
    } else if is_type(t, &SIZE, c"{CGSize=dd}") {
        let s: NSSize = read(this);
        format!("NSSize: {{{}, {}}}", g(s.width), g(s.height))
    } else if is_type(t, &RECT, c"{CGRect={CGPoint=dd}{CGSize=dd}}") {
        let r: NSRect = read(this);
        let (o, s) = (r.origin, r.size);
        format!("NSRect: {{{{{}, {}}}, {{{}, {}}}}}", g(o.x), g(o.y), g(s.width), g(s.height))
    } else if is_type(t, &RANGE, c"{_NSRange=QQ}") {
        let r: NSRange = read(this);
        format!("NSRange: {{{}, {}}}", r.location, r.length)
    } else if is_type(t, &EDGE_INSETS, c"{NSEdgeInsets=dddd}") {
        let e: NSEdgeInsets = read(this);
        format!("NSEdgeInsets: {{{}, {}, {}, {}}}", g(e.top), g(e.left), g(e.bottom), g(e.right))
    } else {
        with_contents(this, |c| describe_bytes(c.map_or(&[][..], |(_, bytes)| bytes)))
    }
}

/// `{length = 4, bytes = 0x2a000000}`; past 24 bytes, the first 16 and the
/// last 8 in groups of four.
fn describe_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = format!("{{length = {}, bytes = 0x", bytes.len());
    if bytes.len() <= 24 {
        for b in bytes {
            let _ = write!(out, "{b:02x}");
        }
        out.push('}');
    } else {
        let group = |out: &mut String, chunk: &[u8]| {
            for word in chunk.chunks(4) {
                for b in word {
                    let _ = write!(out, "{b:02x}");
                }
                out.push(' ');
            }
        };
        group(&mut out, &bytes[..16]);
        out.push_str("... ");
        group(&mut out, &bytes[bytes.len() - 8..]);
        out.push('}');
    }
    out
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSValue"]
    #[ivars = ValueIvars]
    pub(crate) struct NSValueImpl;

    impl NSValueImpl {
        /// A value needs contents: plain `-init` gives nil, as in
        /// Foundation. Subclasses (`NSNumber`) that keep their value
        /// themselves call it to leave this storage empty.
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Option<Retained<Self>> {
            if is_exactly_allocated(&this) {
                None
            } else {
                let this = this.set_ivars(ValueIvars::default());
                // SAFETY: NSObject's designated initializer.
                unsafe { msg_send![super(this), init] }
            }
        }

        #[unsafe(method_id(initWithBytes:objCType:))]
        fn init_with_bytes(this: Allocated<Self>, value: NonNull<c_void>, objc_type: NonNull<c_char>) -> Retained<Self> {
            // SAFETY: the caller passes a C string and a value of that type.
            let (bytes, objc_type) = unsafe { raw_parts(value, objc_type) };
            init(this, bytes, objc_type)
        }

        #[unsafe(method_id(valueWithBytes:objCType:))]
        fn with_bytes(value: NonNull<c_void>, objc_type: NonNull<c_char>) -> Retained<Self> {
            make_raw(value, objc_type)
        }

        #[unsafe(method_id(value:withObjCType:))]
        fn value_with_type(value: NonNull<c_void>, objc_type: NonNull<c_char>) -> Retained<Self> {
            make_raw(value, objc_type)
        }

        #[unsafe(method_id(valueWithPoint:))]
        fn with_point(point: NSPoint) -> Retained<Self> {
            make_from(&point, &POINT)
        }

        #[unsafe(method_id(valueWithSize:))]
        fn with_size(size: NSSize) -> Retained<Self> {
            make_from(&size, &SIZE)
        }

        #[unsafe(method_id(valueWithRect:))]
        fn with_rect(rect: NSRect) -> Retained<Self> {
            make_from(&rect, &RECT)
        }

        #[unsafe(method_id(valueWithRange:))]
        fn with_range(range: NSRange) -> Retained<Self> {
            make_from(&range, &RANGE)
        }

        #[unsafe(method_id(valueWithEdgeInsets:))]
        fn with_edge_insets(insets: NSEdgeInsets) -> Retained<Self> {
            make_from(&insets, &EDGE_INSETS)
        }

        #[unsafe(method_id(valueWithPointer:))]
        fn with_pointer(pointer: *const c_void) -> Retained<Self> {
            make_from(&pointer, &POINTER)
        }

        /// The object is not retained, as the name says. It is kept as a
        /// pointer, `^v`, so it equals `+valueWithPointer:` of the same
        /// address, as in Foundation.
        #[unsafe(method_id(valueWithNonretainedObject:))]
        fn with_nonretained_object(object: *const AnyObject) -> Retained<Self> {
            make_from(&object, &POINTER)
        }

        #[unsafe(method(pointValue))]
        fn point_value(&self) -> NSPoint {
            read(self)
        }

        #[unsafe(method(sizeValue))]
        fn size_value(&self) -> NSSize {
            read(self)
        }

        #[unsafe(method(rectValue))]
        fn rect_value(&self) -> NSRect {
            read(self)
        }

        #[unsafe(method(rangeValue))]
        fn range_value(&self) -> NSRange {
            read(self)
        }

        #[unsafe(method(edgeInsetsValue))]
        fn edge_insets_value(&self) -> NSEdgeInsets {
            read(self)
        }

        #[unsafe(method(pointerValue))]
        fn pointer_value(&self) -> *mut c_void {
            read(self)
        }

        #[unsafe(method(nonretainedObjectValue))]
        fn nonretained_object_value(&self) -> *mut AnyObject {
            read(self)
        }

        #[unsafe(method(objCType))]
        fn objc_type(&self) -> NonNull<c_char> {
            NonNull::new(self.ivars().objc_type.as_ptr().cast_mut()).expect("non-null")
        }

        #[unsafe(method(getValue:))]
        fn get_value(&self, value: NonNull<c_void>) {
            let bytes = &self.ivars().bytes;
            // SAFETY: the caller passes room for a value of this type.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), value.as_ptr().cast::<u8>(), bytes.len()) };
        }

        /// A subclass is read through its `-getValue:`.
        #[unsafe(method(getValue:size:))]
        fn get_value_size(&self, value: NonNull<c_void>, size: NSUInteger) {
            with_contents(self, |contents| {
                let (objc_type, bytes) = contents.unwrap_or((c"", &[]));
                if size != bytes.len() {
                    panic!(
                        "Cannot get value with size {size}. The type encoded as {} is expected to be {} bytes",
                        objc_type.to_string_lossy(),
                        bytes.len()
                    );
                }
                // SAFETY: the caller passes room for `size` bytes.
                unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), value.as_ptr().cast::<u8>(), size) };
            });
        }

        #[unsafe(method(isEqualToValue:))]
        fn is_equal_to_value(&self, other: &NSValue) -> bool {
            values_equal(self, other)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| {
                let value = is_exactly(other, &crate::NSVALUE)
                    || is_exactly(other, &crate::NSNUMBER)
                    || util::is_kind(other, NSValue::class());
                value && values_equal(self, other)
            })
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            with_contents(self, |c| hash_bytes(c.map_or(&[][..], |(_, bytes)| bytes)))
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&describe(self))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            // Immutable: a copy is the same object.
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSValueImpl {}
);

#[cfg(test)]
mod tests {
    use super::layout;

    fn size(enc: &str) -> usize {
        layout(enc.as_bytes()).expect(enc).0
    }

    #[test]
    fn sizes_follow_c_layout() {
        assert_eq!(size("{Foo=cd}"), 16);
        assert_eq!(size("{Bar=ci}"), 8);
        assert_eq!(size("[3s]"), 6);
        assert_eq!(size("{Baz=c[3c]}"), 4);
        assert_eq!(size("(U=cd)"), 8);
        assert_eq!(size("{Q=B}"), 1);
        assert_eq!(size("{E=}"), 0);
        assert_eq!(size("^{Opaque}"), 8);
        assert_eq!(size("{CGRect={CGPoint=dd}{CGSize=dd}}"), 32);
        assert_eq!(size("{Named=\"a\"i\"b\"c}"), 8);
        assert_eq!(size("@\"NSString\""), 8);
        assert_eq!(size("r^v"), 8);
    }
}
