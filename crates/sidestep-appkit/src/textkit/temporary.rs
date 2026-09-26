//! A layout manager's temporary attributes: attributes over character
//! ranges that the layout manager keeps apart from the text storage (find
//! highlights, a syntax colorer's marks), as AppKit's does (measured on
//! macOS by `conformance/tests/text_layout.rs`).
//!
//! They are runs, in order, not overlapping, each with a non-empty
//! dictionary; the text between runs has none. Setting, adding or
//! removing over a range rewrites the runs it covers; an edit of the text
//! keeps the parts of runs before and after it (those after moved along)
//! and drops the part it replaced, so text typed into a run has none.
//! Only a background color is drawn (under the text, over the storage's
//! own backgrounds).

use std::ops::Range;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::NSString;

use super::attrs::{self, Dict};

#[derive(Default)]
pub(crate) struct Temporary {
    runs: Vec<(Range<usize>, Retained<Dict>)>,
}

impl Temporary {
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// The attributes at `index` and the range they cover (a gap between
    /// runs has none), in text `len` long.
    pub fn at(&self, index: usize, len: usize) -> (Option<Retained<Dict>>, Range<usize>) {
        let i = self.runs.partition_point(|(r, _)| r.end <= index);
        match self.runs.get(i) {
            Some((r, d)) if r.start <= index => (Some(d.clone()), r.clone()),
            next => {
                let start = i.checked_sub(1).map_or(0, |p| self.runs[p].0.end);
                let end = next.map_or(len.max(index), |(r, _)| r.start);
                (None, start..end)
            }
        }
    }

    /// The runs meeting `range`, clipped to it.
    pub fn in_range(&self, range: Range<usize>) -> impl Iterator<Item = (Range<usize>, &Retained<Dict>)> {
        let first = self.runs.partition_point(|(r, _)| r.end <= range.start);
        self.runs[first..]
            .iter()
            .take_while(move |(r, _)| r.start < range.end)
            .map(move |(r, d)| (r.start.max(range.start)..r.end.min(range.end), d))
    }

    /// Give each stretch of `range` the attributes `f` makes of its own
    /// (none for the gaps between runs); none or an empty dictionary leaves
    /// it without.
    pub fn map(&mut self, range: Range<usize>, mut f: impl FnMut(Option<&Dict>) -> Option<Retained<Dict>>) {
        if range.is_empty() {
            return;
        }
        let mut out: Vec<(Range<usize>, Retained<Dict>)> = Vec::with_capacity(self.runs.len() + 2);
        let mut at = range.start;
        for (r, d) in std::mem::take(&mut self.runs) {
            if r.end <= range.start || r.start >= range.end {
                if r.start >= range.end && at < range.end {
                    push_mapped(&mut out, at..range.end, None, &mut f);
                    at = range.end;
                }
                push(&mut out, r, d);
                continue;
            }
            if r.start < range.start {
                push(&mut out, r.start..range.start, d.clone());
            }
            if at < r.start {
                push_mapped(&mut out, at..r.start, None, &mut f);
            }
            let (s, e) = (r.start.max(range.start), r.end.min(range.end));
            push_mapped(&mut out, s..e, Some(&d), &mut f);
            at = e;
            if r.end > range.end {
                push(&mut out, range.end..r.end, d);
            }
        }
        if at < range.end {
            push_mapped(&mut out, at..range.end, None, &mut f);
        }
        self.runs = out;
    }

    /// The text's `old` range became `added` units.
    pub fn edited(&mut self, old: Range<usize>, added: usize) {
        if self.runs.is_empty() {
            return;
        }
        let shift = |x: usize| x - old.len() + added;
        let mut out = Vec::with_capacity(self.runs.len() + 1);
        for (r, d) in std::mem::take(&mut self.runs) {
            if r.end <= old.start {
                out.push((r, d));
            } else if r.start >= old.end {
                out.push((shift(r.start)..shift(r.end), d));
            } else {
                if r.start < old.start {
                    out.push((r.start..old.start, d.clone()));
                }
                if r.end > old.end {
                    out.push((shift(old.end)..shift(r.end), d));
                }
            }
        }
        self.runs = out;
    }

    pub fn clear(&mut self) {
        self.runs.clear();
    }
}

fn push(out: &mut Vec<(Range<usize>, Retained<Dict>)>, r: Range<usize>, d: Retained<Dict>) {
    if r.is_empty() || d.count() == 0 {
        return;
    }
    match out.last_mut() {
        Some((last, ld)) if last.end == r.start && (std::ptr::eq(&**ld, &*d) || ld.isEqualToDictionary(&d)) => {
            last.end = r.end;
        }
        _ => out.push((r, d)),
    }
}

fn push_mapped(
    out: &mut Vec<(Range<usize>, Retained<Dict>)>,
    r: Range<usize>,
    d: Option<&Dict>,
    f: &mut impl FnMut(Option<&Dict>) -> Option<Retained<Dict>>,
) {
    if let Some(new) = f(d) {
        push(out, r, new);
    }
}

/// `base` (none: empty) with `extra`'s entries.
pub(crate) fn added(base: Option<&Dict>, extra: &Dict) -> Retained<Dict> {
    match base {
        Some(b) => attrs::merged(b, extra),
        None => {
            use objc2::Message;
            extra.retain()
        }
    }
}

/// `base` with `key` set to `value` (or taken out, for none).
pub(crate) fn with(base: Option<&Dict>, key: &NSString, value: Option<&AnyObject>) -> Option<Retained<Dict>> {
    match base {
        Some(b) => Some(attrs::with_value(b, key, value)),
        None => value.map(|v| attrs::with_value(&Dict::new(), key, Some(v))),
    }
}
