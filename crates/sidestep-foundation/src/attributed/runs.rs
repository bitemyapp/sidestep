//! The attribute runs of an attributed string.
//!
//! A run is a stretch of UTF-16 units sharing one attribute dictionary. Runs
//! are kept in order with their start, so finding the run at an index is a
//! binary search and appending is amortized O(1). An edit rebuilds just the
//! runs it touches (and one neighbour on each side, to merge with) and puts
//! them back with a single splice, so its cost is the splice plus shifting
//! the starts of the runs after it; restyling without changing lengths
//! shifts nothing. The representation stays private to this module so it
//! can become chunked if long, heavily styled documents need that.
//!
//! Adjacent runs merge only when they hold the very same dictionary object,
//! as on macOS: separately built dictionaries with equal contents stay
//! separate runs (the longest-effective-range queries look past that).

use objc2::rc::Retained;

use super::Dict;

pub(crate) struct Run {
    pub start: usize,
    pub len: usize,
    pub attrs: Retained<Dict>,
}

impl Run {
    pub(crate) fn end(&self) -> usize {
        self.start + self.len
    }
}

#[derive(Default)]
pub(crate) struct RunList {
    runs: Vec<Run>,
}

fn same(a: &Dict, b: &Dict) -> bool {
    std::ptr::eq(a, b)
}

/// A stretch of new runs being assembled: lengths and dictionaries, merged
/// as they are added.
#[derive(Default)]
struct Pieces(Vec<(usize, Retained<Dict>)>);

impl Pieces {
    fn push(&mut self, len: usize, attrs: Retained<Dict>) {
        if len == 0 {
            return;
        }
        match self.0.last_mut() {
            Some((l, a)) if same(a, &attrs) => *l += len,
            _ => self.0.push((len, attrs)),
        }
    }
}

impl RunList {
    /// One run of `len` units, or none for empty text.
    pub(crate) fn single(len: usize, attrs: Retained<Dict>) -> Self {
        let mut list = RunList::default();
        list.append(len, attrs);
        list
    }

    pub(crate) fn len(&self) -> usize {
        self.runs.last().map_or(0, Run::end)
    }

    pub(crate) fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// The index of the run holding unit `i`, which must be in bounds.
    #[inline]
    pub(crate) fn find(&self, i: usize) -> usize {
        debug_assert!(i < self.len());
        self.runs.partition_point(|r| r.end() <= i)
    }

    /// The run holding unit `i`.
    pub(crate) fn at(&self, i: usize) -> &Run {
        &self.runs[self.find(i)]
    }

    /// Append `len` units with `attrs`, merging with the last run if it holds
    /// the same dictionary.
    pub(crate) fn append(&mut self, len: usize, attrs: Retained<Dict>) {
        if len == 0 {
            return;
        }
        if let Some(last) = self.runs.last_mut()
            && same(&last.attrs, &attrs)
        {
            last.len += len;
            return;
        }
        let start = self.len();
        self.runs.push(Run { start, len, attrs });
    }

    /// The attributes new text at `range` takes when it replaces that
    /// range: those of its first unit, or for an insertion those of the unit
    /// before it (or after it, at the very start). `None` for empty text.
    pub(crate) fn inherited(&self, loc: usize, len: usize) -> Option<Retained<Dict>> {
        let total = self.len();
        let at = if len > 0 {
            loc
        } else if loc > 0 {
            loc - 1
        } else if total > 0 {
            0
        } else {
            return None;
        };
        Some(self.at(at.min(total - 1)).attrs.clone())
    }

    /// Rebuild the runs `lo..=hi` as `pieces`, then shift the starts of the
    /// runs after them by `delta`.
    fn rebuild(&mut self, lo: usize, hi: usize, pieces: Pieces, delta: isize) {
        let mut pos = self.runs[lo].start;
        let new: Vec<Run> = pieces
            .0
            .into_iter()
            .map(|(len, attrs)| {
                let run = Run { start: pos, len, attrs };
                pos += len;
                run
            })
            .collect();
        let after = lo + new.len();
        self.runs.splice(lo..=hi, new);
        if delta != 0 {
            for run in &mut self.runs[after..] {
                run.start = run.start.wrapping_add_signed(delta);
            }
        }
    }

    /// Replace the units `loc..loc + len` with runs: `new` gives their
    /// lengths and dictionaries in order.
    pub(crate) fn splice(&mut self, loc: usize, len: usize, new: &[(usize, Retained<Dict>)]) {
        let added: usize = new.iter().map(|(l, _)| l).sum();
        if self.runs.is_empty() {
            for (l, attrs) in new {
                self.append(*l, attrs.clone());
            }
            return;
        }
        let n = self.runs.len();
        // The runs the edit touches, and a neighbour on each side.
        let first = if loc < self.len() { self.find(loc) } else { n - 1 };
        let last = if len > 0 { self.find(loc + len - 1) } else { first };
        let (lo, hi) = (first.saturating_sub(1), (last + 1).min(n - 1));
        let end = loc + len;
        let mut pieces = Pieces::default();
        for k in lo..=hi {
            let run = &self.runs[k];
            if k < first || k > last {
                pieces.push(run.len, run.attrs.clone());
                continue;
            }
            if run.start < loc {
                pieces.push(loc.min(run.end()) - run.start, run.attrs.clone());
            }
            if k == last {
                for (l, attrs) in new {
                    pieces.push(*l, attrs.clone());
                }
                if run.end() > end {
                    pieces.push(run.end() - end.max(run.start), run.attrs.clone());
                }
            }
        }
        self.rebuild(lo, hi, pieces, added as isize - len as isize);
    }

    /// Replace the units `loc..loc + len` with `new_len` units carrying
    /// `attrs`.
    pub(crate) fn replace(&mut self, loc: usize, len: usize, new_len: usize, attrs: Retained<Dict>) {
        self.splice(loc, len, &[(new_len, attrs)]);
    }

    /// Give the units `loc..loc + len` the attributes `attrs`.
    pub(crate) fn set(&mut self, loc: usize, len: usize, attrs: Retained<Dict>) {
        if len > 0 {
            self.replace(loc, len, len, attrs);
        }
    }

    /// Replace each dictionary in `loc..loc + len` with `f` of it, which is
    /// asked once per run; runs that shared a dictionary should get a shared
    /// result, which `f` arranges (see `Recv::map_attrs`).
    pub(crate) fn map(&mut self, loc: usize, len: usize, mut f: impl FnMut(&Dict) -> Retained<Dict>) {
        if len == 0 {
            return;
        }
        let n = self.runs.len();
        let (first, last) = (self.find(loc), self.find(loc + len - 1));
        let (lo, hi) = (first.saturating_sub(1), (last + 1).min(n - 1));
        let end = loc + len;
        let mut pieces = Pieces::default();
        for k in lo..=hi {
            let run = &self.runs[k];
            if k < first || k > last {
                pieces.push(run.len, run.attrs.clone());
                continue;
            }
            let (s, e) = (run.start.max(loc), run.end().min(end));
            pieces.push(s - run.start, run.attrs.clone());
            pieces.push(e - s, f(&run.attrs));
            pieces.push(run.end() - e, run.attrs.clone());
        }
        self.rebuild(lo, hi, pieces, 0);
    }

    /// The runs that hold any of the units `loc..loc + len`.
    pub(crate) fn covering(&self, loc: usize, len: usize) -> &[Run] {
        if len == 0 {
            return &[];
        }
        &self.runs[self.find(loc)..=self.find(loc + len - 1)]
    }

    /// The runs of `loc..loc + len`, clipped, with starts relative to `loc`.
    pub(crate) fn slice(&self, loc: usize, len: usize) -> RunList {
        let mut out = RunList::default();
        if len == 0 {
            return out;
        }
        let end = loc + len;
        for run in &self.runs[self.find(loc)..] {
            if run.start >= end {
                break;
            }
            let (s, e) = (run.start.max(loc), run.end().min(end));
            out.append(e - s, run.attrs.clone());
        }
        out
    }

    /// The runs as (length, dictionary) pairs, for [`RunList::splice`].
    pub(crate) fn pieces(&self) -> Vec<(usize, Retained<Dict>)> {
        self.runs.iter().map(|r| (r.len, r.attrs.clone())).collect()
    }
}

#[cfg(test)]
mod tests {
    use objc2_foundation::{NSDictionary, NSString};

    use super::*;

    fn dicts(n: usize) -> Vec<Retained<Dict>> {
        (0..n)
            .map(|i| {
                let key = NSString::from_str("k");
                let value = NSString::from_str(&i.to_string());
                NSDictionary::from_slices(&[&*key], &[value.as_ref()])
            })
            .collect()
    }

    /// Runs as (start, len, which dictionary).
    fn shape(list: &RunList, d: &[Retained<Dict>]) -> Vec<(usize, usize, usize)> {
        let mut pos = 0;
        list.runs
            .iter()
            .map(|r| {
                assert_eq!(r.start, pos, "starts are contiguous");
                pos += r.len;
                (r.start, r.len, d.iter().position(|x| same(x, &r.attrs)).unwrap())
            })
            .collect()
    }

    #[test]
    fn edits() {
        let d = dicts(4);
        let mut list = RunList::single(6, d[0].clone());
        list.set(2, 2, d[1].clone());
        assert_eq!(shape(&list, &d), [(0, 2, 0), (2, 2, 1), (4, 2, 0)]);
        list.replace(3, 0, 3, d[2].clone());
        assert_eq!(shape(&list, &d), [(0, 2, 0), (2, 1, 1), (3, 3, 2), (6, 1, 1), (7, 2, 0)]);
        // Deleting what separates two runs of one dictionary merges them.
        list.replace(2, 5, 0, d[3].clone());
        assert_eq!(shape(&list, &d), [(0, 4, 0)]);
        list.replace(4, 0, 2, d[1].clone());
        assert_eq!(shape(&list, &d), [(0, 4, 0), (4, 2, 1)]);
        list.map(1, 4, |old| if same(old, &d[0]) { d[2].clone() } else { d[3].clone() });
        assert_eq!(shape(&list, &d), [(0, 1, 0), (1, 3, 2), (4, 1, 3), (5, 1, 1)]);
        list.replace(0, 6, 0, d[0].clone());
        assert_eq!(shape(&list, &d), []);
        list.replace(0, 0, 3, d[1].clone());
        assert_eq!(shape(&list, &d), [(0, 3, 1)]);
        list.splice(1, 1, &[(1, d[2].clone()), (2, d[3].clone())]);
        assert_eq!(shape(&list, &d), [(0, 1, 1), (1, 1, 2), (2, 2, 3), (4, 1, 1)]);
    }
}
