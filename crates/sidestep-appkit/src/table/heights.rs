//! Row heights: where each row starts and which row is at a given height,
//! in constant time for tables whose rows are all alike and logarithmic
//! time, through a Fenwick tree, for tables whose delegate sizes each row.
//! Rows inserted, removed or moved keep the other rows' heights, so the
//! delegate is asked only about new rows.

/// The rows' heights, each with the intercell spacing added.
pub(crate) enum Heights {
    /// Every row the same.
    Uniform { count: usize, height: f64 },
    /// Row by row: a Fenwick tree over the heights.
    Varied(Fenwick),
}

impl Heights {
    pub(crate) fn count(&self) -> usize {
        match self {
            Heights::Uniform { count, .. } => *count,
            Heights::Varied(f) => f.len(),
        }
    }

    /// Where row `row` starts.
    pub(crate) fn start(&self, row: usize) -> f64 {
        match self {
            Heights::Uniform { height, .. } => row as f64 * height,
            Heights::Varied(f) => f.prefix(row),
        }
    }

    pub(crate) fn height(&self, row: usize) -> f64 {
        match self {
            Heights::Uniform { height, .. } => *height,
            Heights::Varied(f) => f.get(row),
        }
    }

    /// The height of all rows.
    pub(crate) fn total(&self) -> f64 {
        self.start(self.count())
    }

    /// Rows inserted at `at`, of the heights given (rows alike ignore
    /// them).
    pub(crate) fn insert(&mut self, at: usize, heights: &[f64]) {
        match self {
            Heights::Uniform { count, .. } => *count += heights.len(),
            Heights::Varied(f) => f.splice(at, 0, heights),
        }
    }

    /// `n` rows removed from `at` on.
    pub(crate) fn remove(&mut self, at: usize, n: usize) {
        match self {
            Heights::Uniform { count, .. } => *count = count.saturating_sub(n),
            Heights::Varied(f) => f.splice(at, n, &[]),
        }
    }

    /// Row `from` moved to `to`.
    pub(crate) fn move_row(&mut self, from: usize, to: usize) {
        if let Heights::Varied(f) = self
            && from < f.len()
            && to < f.len()
        {
            let h = f.values.remove(from);
            f.values.insert(to, h);
            *f = Fenwick::new(std::mem::take(&mut f.values));
        }
    }

    /// The row whose span holds `y`, if any.
    pub(crate) fn row_at(&self, y: f64) -> Option<usize> {
        if y < 0.0 || y >= self.total() {
            return None;
        }
        match self {
            Heights::Uniform { height, count } => {
                let row = if *height > 0.0 { (y / height) as usize } else { 0 };
                Some(row.min(count.saturating_sub(1)))
            }
            Heights::Varied(f) => Some(f.find(y)),
        }
    }
}

/// A Fenwick (binary indexed) tree of row heights: prefix sums, a point
/// update and a search, each in logarithmic time.
pub(crate) struct Fenwick {
    /// 1-based partial sums.
    tree: Vec<f64>,
    values: Vec<f64>,
}

impl Fenwick {
    /// Built in linear time.
    pub(crate) fn new(values: Vec<f64>) -> Fenwick {
        let mut tree = vec![0.0; values.len() + 1];
        for (i, v) in values.iter().enumerate() {
            let i = i + 1;
            tree[i] += v;
            let parent = i + (i & i.wrapping_neg());
            if parent < tree.len() {
                tree[parent] += tree[i];
            }
        }
        Fenwick { tree, values }
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn get(&self, i: usize) -> f64 {
        self.values[i]
    }

    /// The sum of the first `n` values.
    pub(crate) fn prefix(&self, n: usize) -> f64 {
        let mut i = n.min(self.values.len());
        let mut sum = 0.0;
        while i > 0 {
            sum += self.tree[i];
            i &= i - 1;
        }
        sum
    }

    pub(crate) fn set(&mut self, i: usize, value: f64) {
        let delta = value - self.values[i];
        self.values[i] = value;
        let mut i = i + 1;
        while i < self.tree.len() {
            self.tree[i] += delta;
            i += i & i.wrapping_neg();
        }
    }

    /// Replace `remove` values from `at` on with `insert`, rebuilding the
    /// tree in linear time.
    pub(crate) fn splice(&mut self, at: usize, remove: usize, insert: &[f64]) {
        let at = at.min(self.values.len());
        let end = (at + remove).min(self.values.len());
        let mut values = std::mem::take(&mut self.values);
        values.splice(at..end, insert.iter().copied());
        *self = Fenwick::new(values);
    }

    /// The index whose span holds `y`, for `y` in `0..total`.
    pub(crate) fn find(&self, y: f64) -> usize {
        let n = self.values.len();
        let mut pos = 0;
        let mut rest = y;
        let mut step = n.next_power_of_two();
        while step > 0 {
            let next = pos + step;
            if next <= n && self.tree[next] <= rest {
                pos = next;
                rest -= self.tree[next];
            }
            step >>= 1;
        }
        pos.min(n.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenwick_sums_updates_and_searches() {
        let values: Vec<f64> = (0..37).map(|i| 10.0 + i as f64).collect();
        let mut f = Fenwick::new(values.clone());
        let naive = |values: &[f64], n: usize| values[..n].iter().sum::<f64>();
        for n in 0..=values.len() {
            assert_eq!(f.prefix(n), naive(&values, n));
        }
        // Every point of every row finds that row.
        for row in 0..values.len() {
            let start = naive(&values, row);
            assert_eq!(f.find(start), row);
            assert_eq!(f.find(start + values[row] - 0.5), row);
        }
        f.set(5, 100.0);
        let mut changed = values.clone();
        changed[5] = 100.0;
        for n in 0..=changed.len() {
            assert_eq!(f.prefix(n), naive(&changed, n));
        }
        assert_eq!(f.find(naive(&changed, 5) + 99.0), 5);
        assert_eq!(f.find(naive(&changed, 6)), 6);
    }

    #[test]
    fn uniform_and_varied_heights_agree() {
        let uniform = Heights::Uniform { count: 10, height: 22.0 };
        let varied = Heights::Varied(Fenwick::new(vec![22.0; 10]));
        for h in [&uniform, &varied] {
            assert_eq!(h.start(3), 66.0);
            assert_eq!(h.total(), 220.0);
            assert_eq!(h.row_at(65.9), Some(2));
            assert_eq!(h.row_at(66.0), Some(3));
            assert_eq!(h.row_at(-1.0), None);
            assert_eq!(h.row_at(220.0), None);
        }
        let empty = Heights::Varied(Fenwick::new(Vec::new()));
        assert_eq!((empty.total(), empty.row_at(0.0)), (0.0, None));
    }

    #[test]
    fn rows_come_and_go() {
        let mut h = Heights::Varied(Fenwick::new(vec![10.0, 20.0, 30.0]));
        h.insert(1, &[5.0]);
        assert_eq!((h.count(), h.start(2), h.total()), (4, 15.0, 65.0));
        h.remove(0, 2);
        assert_eq!((h.count(), h.height(0), h.total()), (2, 20.0, 50.0));
        h.move_row(0, 1);
        assert_eq!((h.height(0), h.start(1), h.row_at(35.0)), (30.0, 30.0, Some(1)));
        let mut u = Heights::Uniform { count: 3, height: 10.0 };
        u.insert(3, &[99.0]);
        u.remove(0, 1);
        assert_eq!((u.count(), u.total()), (3, 30.0));
    }
}
