//! Every non-primitive attributed string method, written once over the
//! primitives.
//!
//! An attributed string's primitives are `string` and
//! `attributesAtIndex:effectiveRange:` for reading, and
//! `replaceCharactersInRange:withString:` and `setAttributes:range:` for
//! writing. When a receiver's class answers them with Sidestep's own
//! implementations, [`Recv`] reads and writes the instance variables
//! directly; otherwise (an app's NSTextStorage, say) it sends the primitive
//! messages, so the subclass sees every read and edit, as on macOS.
//!
//! No `RefCell` borrow is held while a message is sent or a block runs.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{ClassType, msg_send, sel};
use objc2_foundation::{NSAttributedString, NSMutableAttributedString, NSRange, NSString};

use super::runs::RunList;
use super::{AttrIvars, Dict, dicts, ivars_of};
use crate::string::mutable;

fn class_of(obj: &AnyObject) -> *const sidestep_runtime::Class {
    // SAFETY: every object starts with its class pointer.
    unsafe { *(obj as *const AnyObject).cast::<*const sidestep_runtime::Class>() }
}

/// A map keyed by dictionary address.
type PtrMap<V> = HashMap<*const Dict, V, BuildHasherDefault<PtrHash>>;

/// A hash for addresses: a multiply to spread their bits, folded so the
/// low bits (zero, for aligned objects) come out mixed too.
#[derive(Default)]
struct PtrHash(u64);

impl Hasher for PtrHash {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_usize(usize::from(b) ^ self.0 as usize);
        }
    }

    fn write_usize(&mut self, n: usize) {
        let h = (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = h ^ (h >> 29);
    }
}

/// Whether instances of `obj`'s class answer every selector in `sels` with
/// `ours`'s implementation.
fn keeps(obj: &AnyObject, ours: &AnyClass, sels: &[Sel]) -> bool {
    crate::string::install::keeps(obj.class(), ours, sels)
}

/// An attributed string as the generic methods see it.
pub(crate) struct Recv<'a> {
    pub obj: &'a AnyObject,
    reads: Option<&'a AttrIvars>,
    writes: Option<&'a AttrIvars>,
}

impl<'a> Recv<'a> {
    pub(crate) fn new(obj: &'a AnyObject) -> Self {
        let class = class_of(obj);
        if std::ptr::eq(class, &super::NSATTRIBUTEDSTRING) {
            // SAFETY: exactly NSAttributedString.
            let iv = unsafe { ivars_of(obj) };
            return Recv { obj, reads: Some(iv), writes: None };
        }
        if std::ptr::eq(class, &super::NSMUTABLEATTRIBUTEDSTRING) {
            // SAFETY: exactly NSMutableAttributedString.
            let iv = unsafe { ivars_of(obj) };
            return Recv { obj, reads: Some(iv), writes: Some(iv) };
        }
        Self::slow(obj)
    }

    #[cold]
    fn slow(obj: &'a AnyObject) -> Self {
        let reads = keeps(obj, NSAttributedString::class(), &[sel!(string), sel!(attributesAtIndex:effectiveRange:)]);
        let writes = keeps(
            obj,
            NSMutableAttributedString::class(),
            &[sel!(replaceCharactersInRange:withString:), sel!(setAttributes:range:)],
        );
        // SAFETY: a subclass that keeps the primitives keeps their storage.
        let iv = (reads || writes).then(|| unsafe { ivars_of(obj) });
        Recv { obj, reads: iv.filter(|_| reads), writes: iv.filter(|_| writes) }
    }

    /// Whether this receiver's reads come straight from Sidestep's storage.
    pub(crate) fn native(&self) -> Option<&'a AttrIvars> {
        self.reads
    }

    pub(crate) fn len(&self) -> usize {
        match self.reads {
            Some(iv) => iv.runs().len(),
            // SAFETY: every attributed string answers -length.
            None => unsafe { msg_send![self.obj, length] },
        }
    }

    pub(crate) fn string(&self) -> Retained<NSString> {
        match self.reads {
            Some(iv) => iv.text.clone(),
            // SAFETY: a primitive.
            None => unsafe { msg_send![self.obj, string] },
        }
    }

    /// The attributes at `i` and the range of the run holding them.
    pub(crate) fn attrs_at(&self, i: usize) -> (Retained<Dict>, NSRange) {
        match self.reads {
            Some(iv) => {
                let runs = iv.runs();
                let run = runs.at(i);
                (run.attrs.clone(), NSRange::new(run.start, run.len))
            }
            None => {
                let mut range = NSRange::new(0, 0);
                let ptr: *mut NSRange = &mut range;
                // SAFETY: a primitive, with a valid out-parameter.
                let attrs: Retained<Dict> = unsafe { msg_send![self.obj, attributesAtIndex: i, effectiveRange: ptr] };
                (attrs, range)
            }
        }
    }

    /// The runs of `range`, snapshotted.
    pub(crate) fn runs_of(&self, range: NSRange) -> RunList {
        if let Some(iv) = self.reads {
            return iv.runs().slice(range.location, range.length);
        }
        let mut out = RunList::default();
        let mut i = range.location;
        while i < range.end() {
            let (attrs, r) = self.attrs_at(i);
            let end = r.end().min(range.end());
            out.append(end - i, attrs);
            i = end;
        }
        out
    }

    /// The primitive `replaceCharactersInRange:withString:`.
    pub(crate) fn replace_chars(&self, range: NSRange, string: &NSString) {
        match self.writes {
            Some(iv) => native_replace(iv, range, string),
            // SAFETY: a primitive.
            None => unsafe { msg_send![self.obj, replaceCharactersInRange: range, withString: string] },
        }
    }

    /// The primitive `setAttributes:range:`.
    pub(crate) fn set_attrs(&self, range: NSRange, attrs: Option<&Dict>) {
        match self.writes {
            Some(iv) => native_set(iv, range, attrs),
            // SAFETY: a primitive.
            None => unsafe { msg_send![self.obj, setAttributes: attrs, range: range] },
        }
    }

    /// Replace each dictionary in `range` with `f` of it.
    pub(crate) fn map_attrs(&self, method: &str, range: NSRange, mut f: impl FnMut(&Dict) -> Retained<Dict>) {
        check(method, range, self.len());
        match self.writes {
            Some(iv) => {
                // Each distinct dictionary once, so runs that shared one
                // share the result. The new ones are built before the runs
                // are borrowed mutably, since `f` sends messages. The runs
                // keep the old ones alive, so their addresses stay theirs.
                let olds: PtrMap<Retained<Dict>> = {
                    let runs = iv.runs();
                    let covering = runs.covering(range.location, range.length);
                    covering.iter().map(|r| (Retained::as_ptr(&r.attrs), r.attrs.clone())).collect()
                };
                let memo: PtrMap<Retained<Dict>> = olds.into_iter().map(|(key, old)| (key, f(&old))).collect();
                iv.runs_mut().map(range.location, range.length, |old| {
                    let key: *const Dict = old;
                    memo.get(&key).expect("mapped above").clone()
                });
            }
            None => {
                let runs = self.runs_of(range);
                for run in runs.runs() {
                    let new = f(&run.attrs);
                    self.set_attrs(NSRange::new(range.location + run.start, run.len), Some(&new));
                }
            }
        }
    }

    /// `replaceCharactersInRange:withAttributedString:`, the edit behind
    /// append, insert and set.
    pub(crate) fn replace_attributed(&self, range: NSRange, other: &NSAttributedString) {
        check("replaceCharactersInRange:withAttributedString:", range, self.len());
        // Snapshot the argument first: it may be this very string.
        let src = Recv::new(other);
        let src_len = src.len();
        let runs = src.runs_of(NSRange::new(0, src_len));
        let string = src.string();
        match self.writes {
            Some(iv) => {
                mutable::with_text(&string, |text| mutable::edit(&iv.text, range, text));
                iv.runs_mut().splice(range.location, range.length, &runs.pieces());
            }
            None => {
                // SAFETY: -copy of a string is an immutable string, safe from
                // the edit below.
                let string: Retained<NSString> = unsafe { msg_send![&*string, copy] };
                self.replace_chars(range, &string);
                for run in runs.runs() {
                    self.set_attrs(NSRange::new(range.location + run.start, run.len), Some(&run.attrs));
                }
            }
        }
    }

    /// The longest range around `i`, within `limit`, whose attributes are
    /// equal (by value) to those at `i`, and those attributes.
    pub(crate) fn longest(&self, i: usize, limit: NSRange) -> (Retained<Dict>, NSRange) {
        let (attrs, run) = self.attrs_at(i);
        let found = self.widen(run, limit, |d| dicts::equal(d, &attrs));
        (attrs, found)
    }

    /// The value of `key` at `i`, and the longest range within `limit` with
    /// an equal value (nil counting as a value).
    pub(crate) fn longest_value(
        &self,
        key: &NSString,
        i: usize,
        limit: NSRange,
    ) -> (Option<Retained<AnyObject>>, NSRange) {
        let (attrs, run) = self.attrs_at(i);
        let value = attrs.objectForKey(key);
        let found = self.widen(run, limit, |d| dicts::values_equal(d.objectForKey(key).as_deref(), value.as_deref()));
        (value, found)
    }

    /// `run` widened over the runs either side whose attributes are
    /// `alike`, as far as `limit` (whose end wraps, as on macOS, rather
    /// than being checked), then clipped to it as macOS clips it.
    fn widen(&self, run: NSRange, limit: NSRange, alike: impl Fn(&Dict) -> bool) -> NSRange {
        let limit_end = limit.location.wrapping_add(limit.length).min(self.len());
        let (mut start, mut end) = (run.location, run.end());
        while start > limit.location {
            let (prev, r) = self.attrs_at(start - 1);
            if !alike(&prev) {
                break;
            }
            start = r.location;
        }
        while end < limit_end {
            let (next, r) = self.attrs_at(end);
            if !alike(&next) {
                break;
            }
            end = r.end();
        }
        super::clip_to_limit(start..end, limit)
    }

    /// Whether two attributed strings hold equal text and equal attributes
    /// at every position, however their runs are split.
    pub(crate) fn equals(&self, other: &Recv) -> bool {
        if std::ptr::eq(self.obj, other.obj) {
            return true;
        }
        let len = self.len();
        if len != other.len() || !self.string().isEqualToString(&other.string()) {
            return false;
        }
        let mut i = 0;
        while i < len {
            let (a, ra) = self.attrs_at(i);
            let (b, rb) = other.attrs_at(i);
            if !dicts::equal(&a, &b) {
                return false;
            }
            i = ra.end().min(rb.end());
        }
        true
    }
}

/// Panic, as Foundation raises, unless `range` lies within `len` units.
#[track_caller]
pub(crate) fn check(method: &str, range: NSRange, len: usize) {
    if range.location.checked_add(range.length).is_none_or(|end| end > len) {
        panic!(
            "-[NSAttributedString {method}]: Range {{{}, {}}} out of bounds; string length {len}",
            range.location, range.length
        );
    }
}

#[track_caller]
pub(crate) fn check_index(method: &str, i: usize, len: usize) {
    if i >= len {
        panic!("-[NSAttributedString {method}]: index {i} out of bounds; string length {len}");
    }
}

/// The primitive `replaceCharactersInRange:withString:` on Sidestep's
/// storage: new text takes the attributes of the first replaced unit, or
/// for an insertion those of the unit before it.
pub(crate) fn native_replace(iv: &AttrIvars, range: NSRange, string: &NSString) {
    check("replaceCharactersInRange:withString:", range, iv.runs().len());
    let inherited = iv.runs().inherited(range.location, range.length).unwrap_or_else(dicts::empty);
    // The argument may be this string's own text: `with_text` copies
    // anything mutable before the edit.
    let added = mutable::with_text(string, |text| {
        mutable::edit(&iv.text, range, text);
        text.utf16_len
    });
    iv.runs_mut().replace(range.location, range.length, added, inherited);
}

/// The primitive `setAttributes:range:` on Sidestep's storage.
pub(crate) fn native_set(iv: &AttrIvars, range: NSRange, attrs: Option<&Dict>) {
    check("setAttributes:range:", range, iv.runs().len());
    let attrs = dicts::copied(attrs);
    iv.runs_mut().set(range.location, range.length, attrs);
}
