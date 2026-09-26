//! Reading the arguments of the `-init…` family into WTF-8, shared by the
//! immutable string placeholder and `NSMutableString`.

use std::ffi::{CStr, c_char, c_void};

use super::encoding::{self, Decoded};

/// `length` bytes at `bytes` in `encoding`.
///
/// # Safety
/// `bytes` must point to `length` readable bytes (or be anything when
/// `length` is 0).
pub(crate) unsafe fn bytes<'a>(bytes: *const c_void, length: usize, encoding: u32) -> Option<Decoded<'a>> {
    let bytes: &[u8] = if length == 0 {
        &[]
    } else {
        // SAFETY: guaranteed by the caller.
        unsafe { std::slice::from_raw_parts(bytes.cast(), length) }
    };
    encoding::decode(bytes, encoding)
}

/// A NUL-terminated C string in `encoding`; the terminator is as wide as
/// one code unit of the encoding.
///
/// # Safety
/// `s` must point to a terminated string.
pub(crate) unsafe fn c_string<'a>(s: *const c_char, encoding: u32) -> Option<Decoded<'a>> {
    let width = encoding::nul_width(encoding);
    let length = if width == 1 {
        // SAFETY: guaranteed by the caller.
        unsafe { CStr::from_ptr(s) }.to_bytes().len()
    } else {
        let mut n = 0;
        // SAFETY: the string ends with `width` zero bytes, aligned to a
        // code unit.
        while unsafe { std::slice::from_raw_parts(s.cast::<u8>().add(n), width) }.iter().any(|&b| b != 0) {
            n += width;
        }
        n
    };
    // SAFETY: `length` bytes precede the terminator.
    unsafe { bytes(s.cast(), length, encoding) }
}

/// `length` UTF-16 units at `chars`.
///
/// # Safety
/// `chars` must point to `length` units (or be anything when `length` is 0).
pub(crate) unsafe fn units(chars: *const u16, length: usize) -> Decoded<'static> {
    let units: &[u16] = if length == 0 {
        &[]
    } else {
        // SAFETY: guaranteed by the caller.
        unsafe { std::slice::from_raw_parts(chars, length) }
    };
    encoding::from_units(units.iter().copied(), length)
}
