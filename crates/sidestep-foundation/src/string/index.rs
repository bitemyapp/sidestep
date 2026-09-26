//! UTF-16 indices over WTF-8 text.
//!
//! Foundation addresses strings by UTF-16 unit; Sidestep stores UTF-8. For
//! ASCII text the two agree. Otherwise a string remembers the last position
//! it looked up (the cursor), so walking a string with `characterAtIndex:`
//! or `getCharacters:range:` costs O(1) per unit. Strings longer than
//! [`CRUMB`] units also get crumbs on their first random access: the byte
//! offset of every [`CRUMB`]th unit, so any lookup scans fewer than
//! [`CRUMB`] units from a known position, in either direction.
//!
//! Immutable strings share their index between threads, so the cursor is one
//! atomic word and the crumbs are published once with a compare-and-swap and
//! freed with the string. Mutable strings are single-threaded (Foundation's
//! contract, enforced by their `RefCell`), so they use cells, keep the crumbs
//! before an edit, and extend them again on demand.

use std::cell::{Cell, RefCell};
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use super::wtf8::{self, ASCII, Pos};

/// Units between crumbs.
pub(crate) const CRUMB: usize = 64;
/// How far from the cursor a lookup still starts at the cursor.
const NEAR: usize = 2 * CRUMB;

/// A (byte, unit) pair in one word. Both must fit in 32 bits; longer
/// strings go without an index.
#[inline(always)]
fn pack(byte: usize, unit: usize) -> u64 {
    (byte as u64) << 32 | unit as u64
}

#[inline(always)]
fn unpack(p: u64) -> (usize, usize) {
    ((p >> 32) as usize, (p & 0xFFFF_FFFF) as usize)
}

/// Whether text of this length can be indexed at all.
#[inline]
pub(crate) fn indexable(len: usize) -> bool {
    len < u32::MAX as usize
}

/// The index of an immutable string, readable from any thread.
#[derive(Default)]
pub(crate) struct SharedIndex {
    cursor: AtomicU64,
    crumbs: AtomicPtr<Box<[u64]>>,
}

impl SharedIndex {
    /// Free the crumbs. Only for the string's own teardown.
    pub(crate) fn free(&mut self) {
        let crumbs = *self.crumbs.get_mut();
        if !crumbs.is_null() {
            // SAFETY: published by `crumbs` from a Box, and the string is
            // being destroyed, so nothing reads it any more.
            drop(unsafe { Box::from_raw(crumbs) });
        }
    }

    fn crumbs(&self, bytes: &[u8]) -> &[u64] {
        let p = self.crumbs.load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: once published, crumbs live as long as the string.
            return unsafe { &*p };
        }
        let built = Box::into_raw(Box::new(build(bytes, 0, 0, Vec::new()).into_boxed_slice()));
        match self.crumbs.compare_exchange(ptr::null_mut(), built, Ordering::AcqRel, Ordering::Acquire) {
            // SAFETY: just published; freed only with the string.
            Ok(_) => unsafe { &*built },
            Err(other) => {
                // Another thread got there first; its table is the same.
                // SAFETY: `built` was never shared.
                drop(unsafe { Box::from_raw(built) });
                // SAFETY: as above.
                unsafe { &*other }
            }
        }
    }
}

/// The index of a mutable string, which a single thread uses at a time.
#[derive(Default)]
pub(crate) struct LocalIndex {
    cursor: Cell<u64>,
    /// Crumbs for a prefix of the text; extended when a lookup needs more.
    crumbs: RefCell<Vec<u64>>,
}

impl LocalIndex {
    /// The text changed from byte `at` on: forget what lies beyond it.
    pub(crate) fn edited(&mut self, at: usize) {
        let (cursor_byte, _) = unpack(*self.cursor.get_mut());
        if cursor_byte > at {
            self.cursor.set(0);
        }
        let crumbs = self.crumbs.get_mut();
        let keep = crumbs.partition_point(|&p| unpack(p).0 < at);
        crumbs.truncate(keep);
    }

    fn with_crumbs<R>(&self, bytes: &[u8], utf16_len: usize, f: impl FnOnce(&[u64]) -> R) -> R {
        let mut crumbs = self.crumbs.borrow_mut();
        let needed = utf16_len.div_ceil(CRUMB);
        if crumbs.len() < needed {
            let (byte, unit) = crumbs.last().map_or((0, 0), |&p| unpack(p));
            let from = crumbs.len();
            let mut table = std::mem::take(&mut *crumbs);
            // Rebuild from the last good crumb onward.
            table.truncate(from.saturating_sub(1));
            *crumbs = build(bytes, byte, unit, table);
        }
        f(&crumbs)
    }
}

/// Crumbs from (`byte`, `unit`), a character boundary, to the end, appended
/// to `table`. Entry k is the start of the character holding unit k·CRUMB.
fn build(bytes: &[u8], mut byte: usize, mut unit: usize, mut table: Vec<u64>) -> Vec<u64> {
    let mut next = table.len() * CRUMB;
    table.reserve(wtf8::utf16_len(&bytes[byte..]) / CRUMB + 1);
    while byte < bytes.len() {
        let w = wtf8::width(bytes[byte]);
        let units = if w == 4 { 2 } else { 1 };
        while next < unit + units {
            table.push(pack(byte, unit));
            next += CRUMB;
        }
        unit += units;
        byte += w;
    }
    table
}

/// The number of UTF-16 units that start in eight bytes of WTF-8: one per
/// byte that isn't a continuation byte, plus one more per four-byte lead.
#[inline(always)]
fn word_units(w: u64) -> usize {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let continuation = w & !(w << 1) & HIGH;
    let four = w & (w << 1) & (w << 2) & (w << 3) & HIGH;
    8 - continuation.count_ones() as usize + four.count_ones() as usize
}

/// The unit of code point `c` at a position: its high surrogate, or with
/// `low` its low one, for characters outside the Basic Multilingual Plane.
#[inline(always)]
fn unit_of(c: u32, low: bool) -> u16 {
    if c < 0x1_0000 {
        c as u16
    } else {
        let (high, low_half) = wtf8::split_pair(c);
        if low { low_half } else { high }
    }
}

/// Where a string keeps its index, if it has one.
#[derive(Clone, Copy)]
pub(crate) enum IndexRef<'a> {
    None,
    Shared(&'a SharedIndex),
    Local(&'a LocalIndex),
}

/// A string's text as Sidestep's methods see it: WTF-8 bytes, the UTF-16
/// length, flags from [`wtf8`], and the index that speeds up UTF-16
/// addressing.
#[derive(Clone, Copy)]
pub(crate) struct Text<'a> {
    pub bytes: &'a [u8],
    pub utf16_len: usize,
    pub flags: u8,
    pub index: IndexRef<'a>,
}

impl<'a> Text<'a> {
    /// Text with no index, for short-lived or short strings.
    pub(crate) fn plain(bytes: &'a [u8], utf16_len: usize, flags: u8) -> Self {
        Text { bytes, utf16_len, flags, index: IndexRef::None }
    }

    #[inline(always)]
    pub(crate) fn is_ascii(&self) -> bool {
        self.flags & ASCII != 0
    }

    /// The text as `&str`, unless it holds a lone surrogate.
    #[inline]
    pub(crate) fn as_str(&self) -> Option<&'a str> {
        wtf8::as_str(self.bytes, self.flags)
    }

    /// The position of UTF-16 index `i`, which must be at most the length.
    #[inline]
    pub(crate) fn pos(&self, i: usize) -> Pos {
        if self.is_ascii() {
            return Pos::at(i);
        }
        if i == 0 {
            return Pos::at(0);
        }
        if i >= self.utf16_len {
            return Pos::at(self.bytes.len());
        }
        self.pos_slow(i)
    }

    fn cursor(&self) -> Option<(usize, usize)> {
        match self.index {
            IndexRef::None => None,
            IndexRef::Shared(ix) => Some(unpack(ix.cursor.load(Ordering::Relaxed))),
            IndexRef::Local(ix) => Some(unpack(ix.cursor.get())),
        }
    }

    fn remember(&self, byte: usize, unit: usize) {
        match self.index {
            IndexRef::None => {}
            IndexRef::Shared(ix) => ix.cursor.store(pack(byte, unit), Ordering::Relaxed),
            IndexRef::Local(ix) => ix.cursor.set(pack(byte, unit)),
        }
    }

    /// The crumb at or before unit `i`, building crumbs if needed.
    fn crumb(&self, i: usize) -> Option<(usize, usize)> {
        if self.utf16_len <= CRUMB {
            return None;
        }
        match self.index {
            IndexRef::None => None,
            IndexRef::Shared(ix) => Some(unpack(ix.crumbs(self.bytes)[i / CRUMB])),
            IndexRef::Local(ix) => ix.with_crumbs(self.bytes, self.utf16_len, |c| Some(unpack(c[i / CRUMB]))),
        }
    }

    #[inline(never)]
    fn pos_slow(&self, i: usize) -> Pos {
        // Start from the nearest known position: the cursor, a crumb, or an
        // end of the string.
        let (mut byte, mut unit) = match self.cursor() {
            Some((b, u)) if u.abs_diff(i) <= NEAR => (b, u),
            _ => match self.crumb(i) {
                Some(found) => found,
                None if i <= self.utf16_len / 2 => (0, 0),
                None => (self.bytes.len(), self.utf16_len),
            },
        };
        let bytes = self.bytes;
        let pos = if unit <= i {
            // Skip whole words while they end before unit i, then step back
            // to the start of the character the last word cut through.
            while byte + 8 <= bytes.len() {
                let word = u64::from_le_bytes(bytes[byte..byte + 8].try_into().unwrap());
                let units = word_units(word);
                if unit + units > i {
                    break;
                }
                unit += units;
                byte += 8;
            }
            if byte < bytes.len() && bytes[byte] & 0xC0 == 0x80 {
                byte = wtf8::prev_boundary(bytes, byte);
                unit -= if bytes[byte] >= 0xF0 { 2 } else { 1 };
            }
            loop {
                let lead = bytes[byte];
                if lead < 0x80 {
                    if unit == i {
                        break Pos::at(byte);
                    }
                    unit += 1;
                    byte += 1;
                    continue;
                }
                let w = wtf8::width(lead);
                if unit == i {
                    break Pos::at(byte);
                }
                if w == 4 {
                    if unit + 1 == i {
                        break Pos { byte, low: true };
                    }
                    unit += 2;
                } else {
                    unit += 1;
                }
                byte += w;
            }
        } else {
            loop {
                byte = wtf8::prev_boundary(bytes, byte);
                unit -= if bytes[byte] >= 0xF0 { 2 } else { 1 };
                if unit <= i {
                    break Pos { byte, low: unit < i };
                }
            }
        };
        let char_unit = if pos.low { i - 1 } else { i };
        self.remember(pos.byte, char_unit);
        pos
    }

    /// The UTF-16 unit at index `i`, which must be in bounds.
    #[inline]
    pub(crate) fn unit(&self, i: usize) -> u16 {
        if self.is_ascii() {
            return u16::from(self.bytes[i]);
        }
        // Walking a string reads the character at the cursor or the one
        // after it.
        if let Some((byte, unit)) = self.cursor()
            && byte < self.bytes.len()
        {
            let (c, w) = wtf8::decode(self.bytes, byte);
            let units = if c >= 0x1_0000 { 2 } else { 1 };
            if i >= unit && i < unit + units {
                return unit_of(c, i > unit);
            }
            if i == unit + units && byte + w < self.bytes.len() {
                let (next, _) = wtf8::decode(self.bytes, byte + w);
                self.remember(byte + w, i);
                return unit_of(next, false);
            }
        }
        let pos = self.pos(i);
        unit_of(wtf8::decode(self.bytes, pos.byte).0, pos.low)
    }

    /// The UTF-16 index of a byte offset on a character boundary.
    pub(crate) fn utf16_at(&self, byte: usize) -> usize {
        if self.is_ascii() {
            return byte;
        }
        if byte >= self.bytes.len() {
            return self.utf16_len;
        }
        let bytes = self.bytes;
        let (b, u) = match self.cursor() {
            Some((b, u)) if b.abs_diff(byte) <= 4 * NEAR => (b, u),
            _ => self.crumb_before_byte(byte).unwrap_or((0, 0)),
        };
        let unit = if b <= byte { u + wtf8::utf16_len(&bytes[b..byte]) } else { u - wtf8::utf16_len(&bytes[byte..b]) };
        self.remember(byte, unit);
        unit
    }

    fn crumb_before_byte(&self, byte: usize) -> Option<(usize, usize)> {
        if self.utf16_len <= CRUMB {
            return None;
        }
        let find = |c: &[u64]| {
            let k = c.partition_point(|&p| unpack(p).0 <= byte);
            k.checked_sub(1).map(|k| unpack(c[k]))
        };
        match self.index {
            IndexRef::None => None,
            IndexRef::Shared(ix) => find(ix.crumbs(self.bytes)),
            IndexRef::Local(ix) => ix.with_crumbs(self.bytes, self.utf16_len, find),
        }
    }

    /// The positions of a UTF-16 range, which must be in bounds.
    #[inline]
    pub(crate) fn range(&self, loc: usize, len: usize) -> (Pos, Pos) {
        let start = self.pos(loc);
        if len == 0 {
            return (start, start);
        }
        (start, self.pos(loc + len))
    }

    /// Copy the UTF-16 units of `loc..loc + len` into `out`.
    pub(crate) fn copy_units(&self, loc: usize, len: usize, out: &mut [u16]) {
        if len == 0 {
            return;
        }
        if self.is_ascii() {
            for (o, &b) in out[..len].iter_mut().zip(&self.bytes[loc..loc + len]) {
                *o = u16::from(b);
            }
            return;
        }
        let start = self.pos(loc);
        let mut byte = start.byte;
        let mut n = 0;
        if start.low {
            let (c, w) = wtf8::decode(self.bytes, byte);
            out[0] = wtf8::split_pair(c).1;
            byte += w;
            n = 1;
        }
        // The last character walked over, to leave the cursor there.
        let mut last = (byte, loc + n);
        while n < len {
            let (c, w) = wtf8::decode(self.bytes, byte);
            last = (byte, loc + n);
            if c < 0x1_0000 {
                out[n] = c as u16;
                n += 1;
            } else {
                let (high, low) = wtf8::split_pair(c);
                out[n] = high;
                n += 1;
                if n < len {
                    out[n] = low;
                    n += 1;
                }
            }
            byte += w;
        }
        self.remember(last.0, last.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str) {
        let units: Vec<u16> = text.encode_utf16().collect();
        for index in
            [IndexRef::None, IndexRef::Shared(&SharedIndex::default()), IndexRef::Local(&LocalIndex::default())]
        {
            let t = Text { bytes: text.as_bytes(), utf16_len: units.len(), flags: 0, index };
            // Forward, backward and strided, to exercise every start.
            let order: Vec<usize> = (0..units.len())
                .chain((0..units.len()).rev())
                .chain((0..units.len()).map(|i| (i * 7919) % units.len()))
                .collect();
            for &i in &order {
                assert_eq!(t.unit(i), units[i], "unit {i} of {text:?}");
            }
            for (byte, _) in text.char_indices() {
                assert_eq!(t.utf16_at(byte), text[..byte].encode_utf16().count());
            }
            let mut out = vec![0; units.len()];
            for loc in [0, 1, units.len() / 2] {
                let len = units.len().saturating_sub(loc);
                t.copy_units(loc, len, &mut out);
                assert_eq!(&out[..len], &units[loc..]);
            }
        }
    }

    #[test]
    fn indexes_mixed_text() {
        check("a🎉b");
        check(&"héllo 漢字 🎉🎉 wörld ".repeat(40));
        check(&"🎉".repeat(300));
    }

    #[test]
    fn local_index_survives_edits() {
        let mut text = "é".repeat(500).into_bytes();
        let mut ix = LocalIndex::default();
        {
            let t = Text { bytes: &text, utf16_len: 500, flags: 0, index: IndexRef::Local(&ix) };
            assert_eq!(t.pos(400), Pos::at(800));
        }
        text.splice(10..10, *b"xy");
        ix.edited(10);
        let t = Text { bytes: &text, utf16_len: 502, flags: 0, index: IndexRef::Local(&ix) };
        assert_eq!(t.pos(400), Pos::at(798));
        assert_eq!(t.unit(4), 0xE9);
        assert_eq!(t.unit(5), u16::from(b'x'));
        assert_eq!(t.unit(7), 0xE9);
    }
}
