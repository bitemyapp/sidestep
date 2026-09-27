//! Animations as plain data ([`AnimSpec`]) and what they make of a
//! layer's properties at a given time ([`present`]).
//!
//! A `CAAnimation` added to a layer is frozen: `addAnimation:forKey:`
//! copies it and turns the copy into an `AnimSpec`, which both the main
//! thread (for `presentationLayer`, and to know when it ends) and the
//! render thread (to draw it) evaluate the same way. The timing follows
//! `CAMediaTiming` as measured on macOS: an animation's time is
//! `(t − beginTime) · speed + timeOffset`; it is active for its active
//! duration of its parent's time (the simple duration, doubled when it
//! autoreverses, times its repeat count, or its repeat duration), divided
//! by its speed; a time before or after it counts only as its fill mode
//! says; and within it the time wraps around the simple duration, a time
//! at the very end taking the end of the last repeat.

use std::sync::Arc;

use super::math::{self, Bezier, Mat, Spring};
use super::props::{KeyPath, Props, Value};

/// A `CAValueFunction`: numbers an animation interpolates made into the
/// transform it animates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueFn {
    RotateX,
    RotateY,
    RotateZ,
    Scale,
    ScaleX,
    ScaleY,
    ScaleZ,
    Translate,
    TranslateX,
    TranslateY,
    TranslateZ,
}

impl ValueFn {
    pub const NAMES: [(&'static str, ValueFn); 11] = [
        ("rotateX", ValueFn::RotateX),
        ("rotateY", ValueFn::RotateY),
        ("rotateZ", ValueFn::RotateZ),
        ("scale", ValueFn::Scale),
        ("scaleX", ValueFn::ScaleX),
        ("scaleY", ValueFn::ScaleY),
        ("scaleZ", ValueFn::ScaleZ),
        ("translate", ValueFn::Translate),
        ("translateX", ValueFn::TranslateX),
        ("translateY", ValueFn::TranslateY),
        ("translateZ", ValueFn::TranslateZ),
    ];

    pub fn named(name: &str) -> Option<ValueFn> {
        Self::NAMES.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
    }

    /// Whether it takes three numbers (an array of them, measured), not
    /// one.
    pub fn takes_three(self) -> bool {
        matches!(self, ValueFn::Scale | ValueFn::Translate)
    }

    /// The transform for a value (a number, or three).
    pub fn matrix(self, v: &Value) -> Option<Mat> {
        let n = |i: usize| match v {
            Value::Number(x) if i == 0 => Some(*x),
            Value::Numbers(xs) => xs.get(i).copied(),
            _ => None,
        };
        Some(match self {
            ValueFn::RotateX => math::rotation(n(0)?, 1.0, 0.0, 0.0),
            ValueFn::RotateY => math::rotation(n(0)?, 0.0, 1.0, 0.0),
            ValueFn::RotateZ => math::rotation(n(0)?, 0.0, 0.0, 1.0),
            ValueFn::Scale => math::scale(n(0)?, n(1)?, n(2)?),
            ValueFn::ScaleX => math::scale(n(0)?, 1.0, 1.0),
            ValueFn::ScaleY => math::scale(1.0, n(0)?, 1.0),
            ValueFn::ScaleZ => math::scale(1.0, 1.0, n(0)?),
            ValueFn::Translate => math::translation(n(0)?, n(1)?, n(2)?),
            ValueFn::TranslateX => math::translation(n(0)?, 0.0, 0.0),
            ValueFn::TranslateY => math::translation(0.0, n(0)?, 0.0),
            ValueFn::TranslateZ => math::translation(0.0, 0.0, n(0)?),
        })
    }
}

/// What `fillMode` keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Fill {
    #[default]
    Removed,
    Forwards,
    Backwards,
    Both,
}

impl Fill {
    pub fn named(name: &str) -> Fill {
        match name {
            "forwards" => Fill::Forwards,
            "backwards" => Fill::Backwards,
            "both" => Fill::Both,
            _ => Fill::Removed,
        }
    }

    fn forwards(self) -> bool {
        matches!(self, Fill::Forwards | Fill::Both)
    }

    fn backwards(self) -> bool {
        matches!(self, Fill::Backwards | Fill::Both)
    }
}

/// `CAMediaTiming`'s properties.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Timing {
    pub begin: f64,
    pub duration: f64,
    pub speed: f64,
    pub offset: f64,
    pub repeat_count: f64,
    pub repeat_duration: f64,
    pub autoreverses: bool,
    pub fill: Fill,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            begin: 0.0,
            duration: 0.0,
            speed: 1.0,
            offset: 0.0,
            repeat_count: 0.0,
            repeat_duration: 0.0,
            autoreverses: false,
            fill: Fill::Removed,
        }
    }
}

/// Where an animation is at a time: the fraction of its simple duration
/// (0 to 1, before its timing function), and which repeat it is in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Phase {
    pub fraction: f64,
    pub iteration: f64,
}

/// The duration an animation of no duration takes.
pub(crate) const DEFAULT_DURATION: f64 = 0.25;

impl Timing {
    /// The simple duration, `inherited` standing in for none (the default
    /// quarter second at the top, the group's within a group).
    pub fn simple(&self, inherited: f64) -> f64 {
        if self.duration > 0.0 { self.duration } else { inherited }
    }

    /// How long it is active, in its own time.
    pub fn active(&self, d: f64) -> f64 {
        let period = if self.autoreverses { 2.0 * d } else { d };
        if self.repeat_duration > 0.0 {
            self.repeat_duration
        } else if self.repeat_count > 0.0 {
            period * self.repeat_count
        } else {
            period
        }
    }

    /// When it stops being active, in its parent's time (`None`: never,
    /// as for a paused animation or one repeating forever).
    pub fn end(&self, inherited: f64) -> Option<f64> {
        let d = self.simple(inherited);
        let active = self.active(d);
        (self.speed != 0.0 && active.is_finite()).then(|| self.begin + active / self.speed.abs())
    }

    /// Where the animation is at its parent's time `t`, or `None` when it
    /// has no effect then.
    pub fn phase(&self, t: f64, inherited: f64) -> Option<Phase> {
        let d = self.simple(inherited);
        if d <= 0.0 {
            return None;
        }
        let period = if self.autoreverses { 2.0 * d } else { d };
        let active = self.active(d);
        let elapsed = t - self.begin;
        let local = if elapsed < 0.0 {
            if !self.fill.backwards() {
                return None;
            }
            self.offset
        } else if elapsed * self.speed.abs() > active {
            if !self.fill.forwards() {
                return None;
            }
            active * self.speed.signum() + self.offset
        } else {
            elapsed * self.speed + self.offset
        };
        let at_end = elapsed >= 0.0 && elapsed * self.speed.abs() >= active && self.speed != 0.0;
        let mut p = local.rem_euclid(period);
        let mut iteration = (local / period).floor();
        if at_end && p == 0.0 && local > 0.0 {
            // The end of a repeat, not the start of the next.
            p = period;
            iteration -= 1.0;
        }
        if self.autoreverses && p > d {
            p = period - p;
        }
        Some(Phase { fraction: (p / d).clamp(0.0, 1.0), iteration: iteration.max(0.0) })
    }
}

/// How keyframes are interpolated (`calculationMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum CalcMode {
    #[default]
    Linear,
    Discrete,
    Paced,
    Cubic,
    CubicPaced,
}

impl CalcMode {
    pub fn named(name: &str) -> CalcMode {
        match name {
            "discrete" => CalcMode::Discrete,
            "paced" => CalcMode::Paced,
            "cubic" => CalcMode::Cubic,
            "cubicPaced" => CalcMode::CubicPaced,
            _ => CalcMode::Linear,
        }
    }
}

/// A transition's kind (`CATransition`'s `type`) and its direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum TransitionKind {
    #[default]
    Fade,
    MoveIn,
    Push,
    Reveal,
}

/// The side a transition's new content comes from (`subtype`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Direction {
    #[default]
    Left,
    Right,
    Top,
    Bottom,
}

/// What an animation does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AnimKind {
    /// A plain `CAAnimation`: times, but changes nothing.
    None,
    /// `from`, `to` and `by` are of the key's kind, or numbers a value
    /// function makes the transform of.
    Basic {
        path: KeyPath,
        from: Option<Value>,
        to: Option<Value>,
        by: Option<Value>,
        how: Combine,
    },
    Keyframe {
        path: KeyPath,
        values: Vec<Value>,
        key_times: Option<Vec<f64>>,
        functions: Vec<Option<Bezier>>,
        mode: CalcMode,
        how: Combine,
    },
    Spring {
        path: KeyPath,
        from: Option<Value>,
        to: Option<Value>,
        by: Option<Value>,
        spring: Spring,
        how: Combine,
    },
    Transition {
        kind: TransitionKind,
        direction: Direction,
        start: f64,
        end: f64,
    },
    Group(Vec<Arc<AnimSpec>>),
}

/// How a property animation's value meets the model's: added to it, added
/// up over repeats, through a value function.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) struct Combine {
    pub additive: bool,
    pub cumulative: bool,
    pub function: Option<ValueFn>,
}

/// An animation frozen when added to a layer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnimSpec {
    /// Unique per added animation (the render thread tells new
    /// transitions by it).
    pub id: u64,
    pub timing: Timing,
    pub function: Option<Bezier>,
    pub removed_on_completion: bool,
    pub kind: AnimKind,
}

impl AnimSpec {
    /// A key path the animation changes, if it changes one.
    pub fn path(&self) -> Option<KeyPath> {
        match &self.kind {
            AnimKind::Basic { path, .. } | AnimKind::Keyframe { path, .. } | AnimKind::Spring { path, .. } => {
                Some(*path)
            }
            _ => None,
        }
    }

    /// Whether this (or an animation it groups) changes `path`.
    pub fn touches(&self, key: super::props::Key) -> bool {
        match &self.kind {
            AnimKind::Group(children) => children.iter().any(|c| c.touches(key)),
            _ => self.path().is_some_and(|p| p.key == key),
        }
    }

    pub fn is_transition(&self) -> bool {
        matches!(self.kind, AnimKind::Transition { .. })
    }

    /// When it ends, in the layer's time (`None`: never).
    pub fn end(&self) -> Option<f64> {
        self.timing.end(DEFAULT_DURATION)
    }
}

/// What a transition animation is doing at a time: its progress, 0 to 1.
pub(crate) fn transition_progress(spec: &AnimSpec, t: f64) -> Option<(f64, TransitionKind, Direction)> {
    let AnimKind::Transition { kind, direction, start, end } = spec.kind else { return None };
    let phase = spec.timing.phase(t, DEFAULT_DURATION)?;
    let f = spec.function.map_or(phase.fraction, |b| b.value(phase.fraction));
    Some((start + (end - start) * f, kind, direction))
}

/// Apply `anims`, in the order they were added, to `props` at the layer's
/// time `t`.
pub(crate) fn present(props: &mut Props, anims: &[Arc<AnimSpec>], t: f64) {
    for spec in anims {
        apply(props, spec, t, DEFAULT_DURATION);
    }
}

fn apply(props: &mut Props, spec: &AnimSpec, t: f64, inherited: f64) {
    let Some(phase) = spec.timing.phase(t, inherited) else { return };
    let d = spec.timing.simple(inherited);
    let f = spec.function.map_or(phase.fraction, |b| b.value(phase.fraction));
    // Repeats add up only going one way (measured: an animation that
    // autoreverses doesn't accumulate).
    let iteration = if spec.timing.autoreverses { 0.0 } else { phase.iteration };
    match &spec.kind {
        AnimKind::None | AnimKind::Transition { .. } => {}
        AnimKind::Group(children) => {
            // Children run in the group's time, taking its duration when
            // they have none, and stop where it does.
            let local = f * d;
            for child in children {
                apply(props, child, local, d);
            }
        }
        AnimKind::Basic { path, from, to, by, how } => {
            let Some(under) = props.get_path(*path) else { return };
            let Some((a, b)) = ends(&under, from, to, by, how.function.is_some()) else { return };
            finish(props, *path, &under, mix(&a, &b, f), &b, iteration, how);
        }
        AnimKind::Spring { path, from, to, by, spring, how } => {
            let Some(under) = props.get_path(*path) else { return };
            let Some((a, b)) = ends(&under, from, to, by, how.function.is_some()) else { return };
            finish(props, *path, &under, mix(&a, &b, spring.value(f * d)), &b, iteration, how);
        }
        AnimKind::Keyframe { path, values, key_times, functions, mode, how } => {
            // One value (or none) animates nothing (measured).
            let Some(last) = values.last().filter(|_| values.len() > 1) else { return };
            let Some(under) = props.get_path(*path) else { return };
            let v = super::keyframe::value(values, key_times.as_deref(), functions, *mode, f);
            finish(props, *path, &under, v, last, iteration, how);
        }
    }
}

/// `f` of the way from `a` to `b`; values that don't interpolate switch as
/// soon as it leaves `a` (a BOOL, measured), or halfway.
fn mix(a: &Value, b: &Value, f: f64) -> Value {
    a.lerp(b, f).unwrap_or_else(|| {
        let past = if matches!(a, Value::Bool(_)) { f > 0.0 } else { f >= 0.5 };
        if past { b.clone() } else { a.clone() }
    })
}

/// A property animation's value `v` shown: each whole repeat adds the end
/// value when it accumulates (measured), a value function makes it a
/// transform, and an additive one adds to the value under it.
fn finish(props: &mut Props, path: KeyPath, under: &Value, v: Value, end: &Value, iteration: f64, how: &Combine) {
    let mut v = v;
    if how.cumulative
        && iteration > 0.0
        && let Some(step) = end.scaled(iteration)
    {
        v = v.add(&step).unwrap_or(v);
    }
    if let Some(function) = how.function {
        let Some(m) = function.matrix(&v) else { return };
        v = Value::Transform(m);
    }
    if how.additive {
        v = under.add(&v).unwrap_or(v);
    }
    props.set_path(path, v);
}

/// A basic animation's two ends, from what it was given and the value
/// under it (the model's, or what earlier animations made of it), as
/// `CABasicAnimation` pairs them: from–to, from–(from+by), (to−by)–to,
/// from–under, under–to, under–(under+by), or under–under. Through a value
/// function the value under it (a transform) can't be one (macOS raises).
fn ends(
    under: &Value,
    from: &Option<Value>,
    to: &Option<Value>,
    by: &Option<Value>,
    function: bool,
) -> Option<(Value, Value)> {
    Some(match (from, to, by) {
        (Some(f), Some(t), _) => (f.clone(), t.clone()),
        (Some(f), None, Some(b)) => (f.clone(), f.add(b)?),
        (None, Some(t), Some(b)) => (t.sub(b)?, t.clone()),
        _ if function => return None,
        (Some(f), None, None) => (f.clone(), under.clone()),
        (None, Some(t), None) => (under.clone(), t.clone()),
        (None, None, Some(b)) => (under.clone(), under.add(b)?),
        (None, None, None) => (under.clone(), under.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quartzcore::props::{Key, Part};

    fn opacity(from: Option<f64>, to: Option<f64>, by: Option<f64>, timing: Timing) -> AnimSpec {
        AnimSpec {
            id: 1,
            timing,
            function: None,
            removed_on_completion: true,
            kind: AnimKind::Basic {
                path: KeyPath { key: Key::Opacity, part: Part::Whole },
                from: from.map(Value::Number),
                to: to.map(Value::Number),
                by: by.map(Value::Number),
                how: Combine::default(),
            },
        }
    }

    fn at(spec: &AnimSpec, model: f64, t: f64) -> f64 {
        let mut p = Props { opacity: model, ..Props::default() };
        present(&mut p, &[Arc::new(spec.clone())], t);
        p.opacity
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} is not {b}");
    }

    #[test]
    fn timing_matches_macos() {
        // Each row measured through presentation layers on macOS: from 0
        // to 1 over a second, linear, over a model value of 0.5.
        let base = Timing { duration: 1.0, ..Timing::default() };
        let times = [0.0, 0.2, 0.5, 0.9, 1.0, 1.2, 1.5, 1.9, 2.1, 2.5, 3.0, 4.5];
        let row = |timing: Timing, want: [f64; 12]| {
            let spec = opacity(Some(0.0), Some(1.0), None, timing);
            for (t, w) in times.iter().zip(want) {
                assert!((at(&spec, 0.5, *t) - w).abs() < 1e-3, "{timing:?} at {t}: {} is not {w}", at(&spec, 0.5, *t));
            }
        };
        row(Timing { autoreverses: true, ..base }, [0.0, 0.2, 0.5, 0.9, 1.0, 0.8, 0.5, 0.1, 0.5, 0.5, 0.5, 0.5]);
        row(Timing { repeat_count: 2.5, ..base }, [0.0, 0.2, 0.5, 0.9, 0.0, 0.2, 0.5, 0.9, 0.1, 0.5, 0.5, 0.5]);
        row(Timing { repeat_duration: 1.7, ..base }, [0.0, 0.2, 0.5, 0.9, 0.0, 0.2, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]);
        row(Timing { speed: 2.0, ..base }, [0.0, 0.4, 1.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]);
        row(Timing { offset: 0.3, ..base }, [0.3, 0.5, 0.8, 0.2, 0.3, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]);
        row(
            Timing { autoreverses: true, repeat_count: 2.0, ..base },
            [0.0, 0.2, 0.5, 0.9, 1.0, 0.8, 0.5, 0.1, 0.1, 0.5, 1.0, 0.5],
        );
        row(Timing { speed: -1.0, ..base }, [0.0, 0.8, 0.5, 0.1, 0.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]);
        row(Timing { duration: 0.0, ..base }, [0.0, 0.8, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]);
    }

    #[test]
    fn fill_modes_match_macos() {
        // Begins at 1, lasts a second, over a model value of 0.5.
        let times = [0.0, 0.5, 1.0, 1.5, 2.0, 2.5];
        for (fill, want) in [
            (Fill::Removed, [0.5, 0.5, 0.0, 0.5, 1.0, 0.5]),
            (Fill::Forwards, [0.5, 0.5, 0.0, 0.5, 1.0, 1.0]),
            (Fill::Backwards, [0.0, 0.0, 0.0, 0.5, 1.0, 0.5]),
            (Fill::Both, [0.0, 0.0, 0.0, 0.5, 1.0, 1.0]),
        ] {
            let spec =
                opacity(Some(0.0), Some(1.0), None, Timing { begin: 1.0, duration: 1.0, fill, ..Timing::default() });
            for (t, w) in times.iter().zip(want) {
                close(at(&spec, 0.5, *t), w);
            }
        }
    }

    #[test]
    fn from_to_and_by_pair_as_macos_does() {
        let t = Timing { duration: 1.0, ..Timing::default() };
        close(at(&opacity(Some(0.2), None, None, t), 0.5, 0.5), 0.35);
        close(at(&opacity(None, Some(0.8), None, t), 0.5, 0.5), 0.65);
        close(at(&opacity(None, None, Some(0.3), t), 0.5, 0.5), 0.65);
        close(at(&opacity(Some(0.2), None, Some(0.3), t), 0.5, 0.5), 0.35);
        close(at(&opacity(None, Some(0.9), Some(0.3), t), 0.5, 0.5), 0.75);
        close(at(&opacity(None, None, None, t), 0.5, 0.5), 0.5);
    }
}
