//! Keyframe interpolation for `CAKeyframeAnimation`, as measured on macOS:
//!
//! - **linear**: straight between neighbouring values, each segment timed
//!   by the key times (evenly spread without them), with a segment's own
//!   timing function if one is given for it;
//! - **discrete**: each value held for its segment, and without key times
//!   the segments are one per value (so the last value shows for the last
//!   of them); key times may be one more than the values (the last one
//!   the end of the last value's segment);
//! - **paced**: even speed over the whole path, by the distance between
//!   values; with key times given, as linear;
//! - **cubic**: a Kochanek–Bartels spline with no tension, continuity or
//!   bias (Catmull–Rom), each value's tangent half the difference of its
//!   neighbours scaled for uneven segments, and the ends' tangents the
//!   difference to their neighbour;
//! - **cubicPaced**: the cubic spline at even speed along its length.

use super::math::Bezier;
use super::props::Value;
use super::spec::CalcMode;

/// The value `f` (0 to 1, after the animation's own timing function) of
/// the way through `values`.
pub(crate) fn value(
    values: &[Value],
    key_times: Option<&[f64]>,
    functions: &[Option<Bezier>],
    mode: CalcMode,
    f: f64,
) -> Value {
    let n = values.len();
    if n == 1 {
        return values[0].clone();
    }
    // One key time per value; discrete takes one more, for its end.
    let times: Option<Vec<f64>> =
        key_times.filter(|k| k.len() == n || (mode == CalcMode::Discrete && k.len() == n + 1)).map(<[f64]>::to_vec);
    match mode {
        CalcMode::Discrete => {
            let times = times.unwrap_or_else(|| (0..=n).map(|i| i as f64 / n as f64).collect());
            let i = segment(&times, f).min(n - 1);
            values[i].clone()
        }
        CalcMode::Paced if times.is_none() => {
            let times = paced_times(values);
            linear(values, &times, &[], f)
        }
        CalcMode::Linear | CalcMode::Paced => {
            let times = times.unwrap_or_else(|| even(n));
            linear(values, &times, functions, f)
        }
        CalcMode::Cubic | CalcMode::CubicPaced => {
            let paced = mode == CalcMode::CubicPaced && times.is_none();
            let times = times.unwrap_or_else(|| even(n));
            if paced { cubic_paced(values, &times, f) } else { cubic(values, &times, functions, f) }
        }
    }
}

fn even(n: usize) -> Vec<f64> {
    (0..n).map(|i| i as f64 / (n - 1) as f64).collect()
}

/// The segment `f` falls in: `i` where `times[i] <= f < times[i + 1]`.
fn segment(times: &[f64], f: f64) -> usize {
    let last = times.len().saturating_sub(2);
    let mut i = 0;
    while i < last && f >= times[i + 1] {
        i += 1;
    }
    i
}

/// Where `f` is within segment `i`, 0 to 1.
fn local(times: &[f64], i: usize, f: f64) -> f64 {
    let span = times[i + 1] - times[i];
    if span <= 0.0 { 1.0 } else { ((f - times[i]) / span).clamp(0.0, 1.0) }
}

fn linear(values: &[Value], times: &[f64], functions: &[Option<Bezier>], f: f64) -> Value {
    let i = segment(times, f);
    let mut s = local(times, i, f);
    if let Some(Some(b)) = functions.get(i) {
        s = b.value(s);
    }
    values[i].lerp(&values[i + 1], s).unwrap_or_else(|| if s < 1.0 { values[i].clone() } else { values[i + 1].clone() })
}

/// Key times that make the speed even over the distances between values.
pub(crate) fn paced_times(values: &[Value]) -> Vec<f64> {
    let d: Vec<f64> = values.windows(2).map(|w| w[0].distance(&w[1])).collect();
    let total: f64 = d.iter().sum();
    if total <= 0.0 {
        return even(values.len());
    }
    let mut times = Vec::with_capacity(values.len());
    let mut acc = 0.0;
    times.push(0.0);
    for len in d {
        acc += len;
        times.push(acc / total);
    }
    times
}

/// The spline's tangents at each value, per segment: the one leaving a
/// value (index `i`, into segment `i`) and the one arriving at it (into
/// segment `i − 1`).
fn tangents(c: &[Vec<f64>], times: &[f64]) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
    let n = c.len();
    let dim = c[0].len();
    let mut out = vec![vec![0.0; dim]; n];
    let mut inn = vec![vec![0.0; dim]; n];
    for i in 0..n {
        if i == 0 {
            out[0] = (0..dim).map(|k| c[1][k] - c[0][k]).collect();
        } else if i == n - 1 {
            inn[i] = (0..dim).map(|k| c[i][k] - c[i - 1][k]).collect();
        } else {
            let t: Vec<f64> = (0..dim).map(|k| (c[i + 1][k] - c[i - 1][k]) / 2.0).collect();
            let (d0, d1) = (times[i] - times[i - 1], times[i + 1] - times[i]);
            let sum = d0 + d1;
            let (ki, ko) = if sum > 0.0 { (2.0 * d0 / sum, 2.0 * d1 / sum) } else { (1.0, 1.0) };
            inn[i] = t.iter().map(|v| v * ki).collect();
            out[i] = t.iter().map(|v| v * ko).collect();
        }
    }
    (out, inn)
}

fn hermite(p0: &[f64], m0: &[f64], p1: &[f64], m1: &[f64], s: f64) -> Vec<f64> {
    let (s2, s3) = (s * s, s * s * s);
    let h00 = 2.0 * s3 - 3.0 * s2 + 1.0;
    let h10 = s3 - 2.0 * s2 + s;
    let h01 = -2.0 * s3 + 3.0 * s2;
    let h11 = s3 - s2;
    (0..p0.len()).map(|k| h00 * p0[k] + h10 * m0[k] + h01 * p1[k] + h11 * m1[k]).collect()
}

fn cubic(values: &[Value], times: &[f64], functions: &[Option<Bezier>], f: f64) -> Value {
    let Some(c) = values.iter().map(Value::components).collect::<Option<Vec<_>>>() else {
        return linear(values, times, functions, f);
    };
    let (out, inn) = tangents(&c, times);
    let i = segment(times, f);
    let mut s = local(times, i, f);
    if let Some(Some(b)) = functions.get(i) {
        s = b.value(s);
    }
    values[i].with_components(&hermite(&c[i], &out[i], &c[i + 1], &inn[i + 1], s))
}

/// The cubic spline at even speed along its length (measured by sampling).
fn cubic_paced(values: &[Value], times: &[f64], f: f64) -> Value {
    let Some(c) = values.iter().map(Value::components).collect::<Option<Vec<_>>>() else {
        return linear(values, &paced_times(values), &[], f);
    };
    let (out, inn) = tangents(&c, times);
    const STEPS: usize = 64;
    // Arc length at each sample along the whole spline.
    let mut samples: Vec<(usize, f64, Vec<f64>)> = Vec::new();
    for i in 0..c.len() - 1 {
        for k in 0..STEPS {
            let s = k as f64 / STEPS as f64;
            samples.push((i, s, hermite(&c[i], &out[i], &c[i + 1], &inn[i + 1], s)));
        }
    }
    samples.push((c.len() - 2, 1.0, c[c.len() - 1].clone()));
    let mut lengths = vec![0.0];
    for w in samples.windows(2) {
        let d: f64 = w[0].2.iter().zip(&w[1].2).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
        lengths.push(lengths.last().copied().unwrap_or(0.0) + d);
    }
    let total = *lengths.last().unwrap_or(&0.0);
    if total <= 0.0 {
        return values[0].clone();
    }
    let want = f.clamp(0.0, 1.0) * total;
    let j = lengths.partition_point(|l| *l < want).clamp(1, lengths.len() - 1);
    let (l0, l1) = (lengths[j - 1], lengths[j]);
    let u = if l1 > l0 { (want - l0) / (l1 - l0) } else { 0.0 };
    let (i0, s0, _) = &samples[j - 1];
    let (i1, s1, _) = &samples[j];
    let (i, s) = if i0 == i1 { (*i0, s0 + (s1 - s0) * u) } else { (*i0, s0 + (1.0 - s0) * u) };
    values[i].with_components(&hermite(&c[i], &out[i], &c[i + 1], &inn[i + 1], s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nums(v: &[f64]) -> Vec<Value> {
        v.iter().map(|x| Value::Number(*x)).collect()
    }

    fn check(mode: CalcMode, key_times: Option<&[f64]>, want: [f64; 12]) {
        let values = nums(&[0.0, 1.0, 0.5, 0.7]);
        let times = [0.0, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 0.99];
        for (t, w) in times.iter().zip(want) {
            let Value::Number(got) = value(&values, key_times, &[], mode, *t) else { panic!("a number") };
            assert!((got - w).abs() < 1e-3, "{mode:?} {key_times:?} at {t}: {got} is not {w}");
        }
    }

    #[test]
    fn modes_match_macos() {
        // Measured on macOS: values 0, 1, 0.5, 0.7 over a second.
        let kt: &[f64] = &[0.0, 0.1, 0.6, 1.0];
        check(CalcMode::Linear, None, [0.0, 0.15, 0.3, 0.6, 0.9, 0.9, 0.75, 0.6, 0.52, 0.58, 0.64, 0.694]);
        check(CalcMode::Linear, Some(kt), [0.0, 0.5, 1.0, 0.9, 0.8, 0.7, 0.6, 0.5, 0.55, 0.6, 0.65, 0.695]);
        check(CalcMode::Discrete, None, [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.5, 0.5, 0.5, 0.7, 0.7, 0.7]);
        check(CalcMode::Discrete, Some(kt), [0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.5, 0.5, 0.5, 0.5, 0.5]);
        check(CalcMode::Paced, None, [0.0, 0.085, 0.17, 0.34, 0.51, 0.68, 0.85, 0.98, 0.81, 0.64, 0.53, 0.683]);
        check(CalcMode::Paced, Some(kt), [0.0, 0.5, 1.0, 0.9, 0.8, 0.7, 0.6, 0.5, 0.55, 0.6, 0.65, 0.695]);
        check(
            CalcMode::Cubic,
            None,
            [0.0, 0.16434374, 0.34725, 0.708, 0.96075, 0.9848, 0.8, 0.5792, 0.49165, 0.5296, 0.61795, 0.6936945],
        );
        check(
            CalcMode::Cubic,
            Some(kt),
            [0.0, 0.6145833, 1.0, 1.0066667, 0.9, 0.74, 0.58666664, 0.5, 0.503125, 0.55833334, 0.634375, 0.69479686],
        );
    }

    #[test]
    fn cubic_points_take_end_tangents() {
        // Measured: (0, 0), (30, 0), (30, 10), cubic, at a quarter.
        let values = vec![Value::Point([0.0, 0.0]), Value::Point([30.0, 0.0]), Value::Point([30.0, 10.0])];
        let Value::Point(p) = value(&values, None, &[], CalcMode::Cubic, 0.25) else { panic!("a point") };
        assert!((p[0] - 16.875).abs() < 1e-9 && (p[1] + 0.625).abs() < 1e-9, "{p:?}");
        let Value::Point(p) = value(&values, None, &[], CalcMode::Paced, 0.55) else { panic!("a point") };
        assert!((p[0] - 22.0).abs() < 1e-9, "{p:?}");
    }
}
