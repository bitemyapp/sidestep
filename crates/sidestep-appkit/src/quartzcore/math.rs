//! The numbers under Core Animation, with no objects: 4×4 transforms and
//! their decomposition, cubic Bézier timing curves, damped springs, and
//! the media clock.
//!
//! Transforms use CoreGraphics' row-vector convention, as
//! `CATransform3D` does: a point is a row `[x y z 1]` multiplied on the
//! right, so `concat(a, b)` applies `a` and then `b`, and `m41`, `m42`,
//! `m43` are the translation.

/// A `CATransform3D`'s sixteen numbers, row by row.
pub(crate) type Mat = [f64; 16];

pub(crate) const IDENTITY: Mat = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// `a` then `b` (`CATransform3DConcat(a, b)`).
pub(crate) fn concat(a: &Mat, b: &Mat) -> Mat {
    let mut m = [0.0; 16];
    for r in 0..4 {
        for c in 0..4 {
            m[r * 4 + c] = (0..4).map(|k| a[r * 4 + k] * b[k * 4 + c]).sum();
        }
    }
    m
}

pub(crate) fn translation(x: f64, y: f64, z: f64) -> Mat {
    let mut m = IDENTITY;
    m[12] = x;
    m[13] = y;
    m[14] = z;
    m
}

pub(crate) fn scale(x: f64, y: f64, z: f64) -> Mat {
    let mut m = IDENTITY;
    m[0] = x;
    m[5] = y;
    m[10] = z;
    m
}

/// A rotation by `angle` radians about the axis (x, y, z), which needn't
/// be normalized; none for an axis of no length.
pub(crate) fn rotation(angle: f64, x: f64, y: f64, z: f64) -> Mat {
    let len = (x * x + y * y + z * z).sqrt();
    if len == 0.0 || !len.is_finite() {
        return IDENTITY;
    }
    let (x, y, z) = (x / len, y / len, z / len);
    let (s, c) = angle.sin_cos();
    let t = 1.0 - c;
    [
        c + x * x * t,
        x * y * t + z * s,
        x * z * t - y * s,
        0.0,
        x * y * t - z * s,
        c + y * y * t,
        y * z * t + x * s,
        0.0,
        x * z * t + y * s,
        y * z * t - x * s,
        c + z * z * t,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

/// The inverse of `m`, or `None` when it has none.
pub(crate) fn invert(m: &Mat) -> Option<Mat> {
    // Cofactors, as in the standard 4×4 inverse.
    let a = m;
    let mut inv = [0.0; 16];
    inv[0] =
        a[5] * a[10] * a[15] - a[5] * a[11] * a[14] - a[9] * a[6] * a[15] + a[9] * a[7] * a[14] + a[13] * a[6] * a[11]
            - a[13] * a[7] * a[10];
    inv[4] =
        -a[4] * a[10] * a[15] + a[4] * a[11] * a[14] + a[8] * a[6] * a[15] - a[8] * a[7] * a[14] - a[12] * a[6] * a[11]
            + a[12] * a[7] * a[10];
    inv[8] =
        a[4] * a[9] * a[15] - a[4] * a[11] * a[13] - a[8] * a[5] * a[15] + a[8] * a[7] * a[13] + a[12] * a[5] * a[11]
            - a[12] * a[7] * a[9];
    inv[12] =
        -a[4] * a[9] * a[14] + a[4] * a[10] * a[13] + a[8] * a[5] * a[14] - a[8] * a[6] * a[13] - a[12] * a[5] * a[10]
            + a[12] * a[6] * a[9];
    inv[1] =
        -a[1] * a[10] * a[15] + a[1] * a[11] * a[14] + a[9] * a[2] * a[15] - a[9] * a[3] * a[14] - a[13] * a[2] * a[11]
            + a[13] * a[3] * a[10];
    inv[5] =
        a[0] * a[10] * a[15] - a[0] * a[11] * a[14] - a[8] * a[2] * a[15] + a[8] * a[3] * a[14] + a[12] * a[2] * a[11]
            - a[12] * a[3] * a[10];
    inv[9] =
        -a[0] * a[9] * a[15] + a[0] * a[11] * a[13] + a[8] * a[1] * a[15] - a[8] * a[3] * a[13] - a[12] * a[1] * a[11]
            + a[12] * a[3] * a[9];
    inv[13] =
        a[0] * a[9] * a[14] - a[0] * a[10] * a[13] - a[8] * a[1] * a[14] + a[8] * a[2] * a[13] + a[12] * a[1] * a[10]
            - a[12] * a[2] * a[9];
    inv[2] =
        a[1] * a[6] * a[15] - a[1] * a[7] * a[14] - a[5] * a[2] * a[15] + a[5] * a[3] * a[14] + a[13] * a[2] * a[7]
            - a[13] * a[3] * a[6];
    inv[6] =
        -a[0] * a[6] * a[15] + a[0] * a[7] * a[14] + a[4] * a[2] * a[15] - a[4] * a[3] * a[14] - a[12] * a[2] * a[7]
            + a[12] * a[3] * a[6];
    inv[10] =
        a[0] * a[5] * a[15] - a[0] * a[7] * a[13] - a[4] * a[1] * a[15] + a[4] * a[3] * a[13] + a[12] * a[1] * a[7]
            - a[12] * a[3] * a[5];
    inv[14] =
        -a[0] * a[5] * a[14] + a[0] * a[6] * a[13] + a[4] * a[1] * a[14] - a[4] * a[2] * a[13] - a[12] * a[1] * a[6]
            + a[12] * a[2] * a[5];
    inv[3] =
        -a[1] * a[6] * a[11] + a[1] * a[7] * a[10] + a[5] * a[2] * a[11] - a[5] * a[3] * a[10] - a[9] * a[2] * a[7]
            + a[9] * a[3] * a[6];
    inv[7] = a[0] * a[6] * a[11] - a[0] * a[7] * a[10] - a[4] * a[2] * a[11] + a[4] * a[3] * a[10] + a[8] * a[2] * a[7]
        - a[8] * a[3] * a[6];
    inv[11] = -a[0] * a[5] * a[11] + a[0] * a[7] * a[9] + a[4] * a[1] * a[11] - a[4] * a[3] * a[9] - a[8] * a[1] * a[7]
        + a[8] * a[3] * a[5];
    inv[15] = a[0] * a[5] * a[10] - a[0] * a[6] * a[9] - a[4] * a[1] * a[10] + a[4] * a[2] * a[9] + a[8] * a[1] * a[6]
        - a[8] * a[2] * a[5];
    let det = a[0] * inv[0] + a[1] * inv[4] + a[2] * inv[8] + a[3] * inv[12];
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    Some(inv.map(|v| v / det))
}

/// Whether `m` maps the plane to itself with no depth or perspective: what
/// `CATransform3DIsAffine` asks.
pub(crate) fn is_affine(m: &Mat) -> bool {
    m[2] == 0.0
        && m[3] == 0.0
        && m[6] == 0.0
        && m[7] == 0.0
        && m[8] == 0.0
        && m[9] == 0.0
        && m[10] == 1.0
        && m[11] == 0.0
        && m[14] == 0.0
        && m[15] == 1.0
}

/// Where `m` takes the point (x, y) in the plane z = 0, with the
/// perspective divide.
pub(crate) fn apply(m: &Mat, x: f64, y: f64) -> (f64, f64) {
    let px = x * m[0] + y * m[4] + m[12];
    let py = x * m[1] + y * m[5] + m[13];
    let w = x * m[3] + y * m[7] + m[15];
    if w != 0.0 && w != 1.0 && w.is_finite() { (px / w, py / w) } else { (px, py) }
}

/// The affine map `m` gives the plane z = 0 as drawing sees it (an
/// orthographic view: depth is dropped): a, b, c, d, tx, ty. With
/// perspective, the map through three of a unit square's projected
/// corners around `(ox, oy)`, which is exact at those corners.
pub(crate) fn plane_affine(m: &Mat, ox: f64, oy: f64, w: f64, h: f64) -> [f64; 6] {
    if m[3] == 0.0 && m[7] == 0.0 && m[15] == 1.0 {
        return [m[0], m[1], m[4], m[5], m[12], m[13]];
    }
    let (w, h) = (if w.abs() > 1e-9 { w } else { 1.0 }, if h.abs() > 1e-9 { h } else { 1.0 });
    let p0 = apply(m, ox, oy);
    let px = apply(m, ox + w, oy);
    let py = apply(m, ox, oy + h);
    let a = (px.0 - p0.0) / w;
    let b = (px.1 - p0.1) / w;
    let c = (py.0 - p0.0) / h;
    let d = (py.1 - p0.1) / h;
    [a, b, c, d, p0.0 - a * ox - c * oy, p0.1 - b * ox - d * oy]
}

/// A transform taken apart for interpolation: translation, scale, a
/// rotation as a unit quaternion, skews (xy, xz, yz) and perspective.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Parts {
    pub translate: [f64; 3],
    pub scale: [f64; 3],
    pub skew: [f64; 3],
    pub quaternion: [f64; 4],
    pub perspective: [f64; 4],
}

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn combine(a: [f64; 3], b: [f64; 3], sa: f64, sb: f64) -> [f64; 3] {
    [a[0] * sa + b[0] * sb, a[1] * sa + b[1] * sb, a[2] * sa + b[2] * sb]
}

fn length3(a: [f64; 3]) -> f64 {
    dot3(a, a).sqrt()
}

/// `m` taken apart (the unmatrix method of Graphics Gems II, as CSS
/// transforms decompose); `None` for a matrix that can't be (singular).
pub(crate) fn decompose(m: &Mat) -> Option<Parts> {
    if m[15] == 0.0 {
        return None;
    }
    let n: Vec<f64> = m.iter().map(|v| v / m[15]).collect();
    // The perspective part.
    let mut persp = n.clone();
    persp[3] = 0.0;
    persp[7] = 0.0;
    persp[11] = 0.0;
    persp[15] = 1.0;
    let persp: Mat = persp.try_into().ok()?;
    // A singular perspective part can't be taken apart.
    invert(&persp)?;
    let perspective = if n[3] != 0.0 || n[7] != 0.0 || n[11] != 0.0 {
        let rhs = [n[3], n[7], n[11], n[15]];
        let inv = invert(&persp)?;
        // rhs · (inverse transposed), as a row vector.
        let mut p = [0.0; 4];
        for (c, out) in p.iter_mut().enumerate() {
            *out = (0..4).map(|k| rhs[k] * inv[c * 4 + k]).sum();
        }
        p
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    let translate = [n[12], n[13], n[14]];
    let mut row = [[n[0], n[1], n[2]], [n[4], n[5], n[6]], [n[8], n[9], n[10]]];
    let mut scale = [0.0; 3];
    let mut skew = [0.0; 3];
    scale[0] = length3(row[0]);
    if scale[0] == 0.0 {
        return None;
    }
    row[0] = row[0].map(|v| v / scale[0]);
    skew[0] = dot3(row[0], row[1]);
    row[1] = combine(row[1], row[0], 1.0, -skew[0]);
    scale[1] = length3(row[1]);
    if scale[1] == 0.0 {
        return None;
    }
    row[1] = row[1].map(|v| v / scale[1]);
    skew[0] /= scale[1];
    skew[1] = dot3(row[0], row[2]);
    row[2] = combine(row[2], row[0], 1.0, -skew[1]);
    skew[2] = dot3(row[1], row[2]);
    row[2] = combine(row[2], row[1], 1.0, -skew[2]);
    scale[2] = length3(row[2]);
    if scale[2] == 0.0 {
        return None;
    }
    row[2] = row[2].map(|v| v / scale[2]);
    skew[1] /= scale[2];
    skew[2] /= scale[2];
    // A mirror image: flip everything.
    if dot3(row[0], cross3(row[1], row[2])) < 0.0 {
        for i in 0..3 {
            scale[i] = -scale[i];
            row[i] = row[i].map(|v| -v);
        }
    }
    let q = quaternion_of(&row);
    Some(Parts { translate, scale, skew, quaternion: q, perspective })
}

/// The unit quaternion of a rotation matrix given by rows (row vectors).
fn quaternion_of(r: &[[f64; 3]; 3]) -> [f64; 4] {
    // For row vectors the matrix is the transpose of the column form.
    let (m00, m01, m02) = (r[0][0], r[1][0], r[2][0]);
    let (m10, m11, m12) = (r[0][1], r[1][1], r[2][1]);
    let (m20, m21, m22) = (r[0][2], r[1][2], r[2][2]);
    let trace = m00 + m11 + m22;
    let (x, y, z, w);
    if trace > 0.0 {
        let s = 0.5 / (trace + 1.0).sqrt();
        w = 0.25 / s;
        x = (m21 - m12) * s;
        y = (m02 - m20) * s;
        z = (m10 - m01) * s;
    } else if m00 > m11 && m00 > m22 {
        let s = 2.0 * (1.0 + m00 - m11 - m22).sqrt();
        w = (m21 - m12) / s;
        x = 0.25 * s;
        y = (m01 + m10) / s;
        z = (m02 + m20) / s;
    } else if m11 > m22 {
        let s = 2.0 * (1.0 + m11 - m00 - m22).sqrt();
        w = (m02 - m20) / s;
        x = (m01 + m10) / s;
        y = 0.25 * s;
        z = (m12 + m21) / s;
    } else {
        let s = 2.0 * (1.0 + m22 - m00 - m11).sqrt();
        w = (m10 - m01) / s;
        x = (m02 + m20) / s;
        y = (m12 + m21) / s;
        z = 0.25 * s;
    }
    [x, y, z, w]
}

/// The rotation matrix (row vectors) of a unit quaternion.
fn rotation_of(q: [f64; 4]) -> Mat {
    let [x, y, z, w] = q;
    // Column form, transposed into rows.
    let m00 = 1.0 - 2.0 * (y * y + z * z);
    let m01 = 2.0 * (x * y - z * w);
    let m02 = 2.0 * (x * z + y * w);
    let m10 = 2.0 * (x * y + z * w);
    let m11 = 1.0 - 2.0 * (x * x + z * z);
    let m12 = 2.0 * (y * z - x * w);
    let m20 = 2.0 * (x * z - y * w);
    let m21 = 2.0 * (y * z + x * w);
    let m22 = 1.0 - 2.0 * (x * x + y * y);
    [m00, m10, m20, 0.0, m01, m11, m21, 0.0, m02, m12, m22, 0.0, 0.0, 0.0, 0.0, 1.0]
}

/// The transform `parts` describe.
pub(crate) fn recompose(p: &Parts) -> Mat {
    // perspective · translate · rotate · skew · scale, in column form;
    // in rows: scale, skew, rotate, translate, then perspective.
    let mut m = IDENTITY;
    m[3] = p.perspective[0];
    m[7] = p.perspective[1];
    m[11] = p.perspective[2];
    m[15] = p.perspective[3];
    let s = scale(p.scale[0], p.scale[1], p.scale[2]);
    let mut skew = IDENTITY;
    if p.skew[2] != 0.0 {
        let mut k = IDENTITY;
        k[9] = p.skew[2];
        skew = concat(&k, &skew);
    }
    if p.skew[1] != 0.0 {
        let mut k = IDENTITY;
        k[8] = p.skew[1];
        skew = concat(&k, &skew);
    }
    if p.skew[0] != 0.0 {
        let mut k = IDENTITY;
        k[4] = p.skew[0];
        skew = concat(&k, &skew);
    }
    let r = rotation_of(p.quaternion);
    let t = translation(p.translate[0], p.translate[1], p.translate[2]);
    let mut out = concat(&s, &skew);
    out = concat(&out, &r);
    out = concat(&out, &t);
    concat(&out, &m)
}

/// A plane transform taken apart as the CSS Transforms specification's
/// 2-D matrix decomposition does: translation, scale (a mirror image
/// flipping the axis with the smaller unit-vector dot product), a rotation
/// angle in degrees, and the 2 × 2 remainder. The matrix is
/// `scale · rotation · remainder`, then the translation.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Plane {
    translate: [f64; 2],
    scale: [f64; 2],
    angle: f64,
    rest: [f64; 4],
}

fn decompose_plane(m: &Mat) -> Option<Plane> {
    let (mut r0x, mut r0y, mut r1x, mut r1y) = (m[0], m[1], m[4], m[5]);
    let det = r0x * r1y - r0y * r1x;
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let mut scale = [r0x.hypot(r0y), r1x.hypot(r1y)];
    if det < 0.0 {
        if r0x < r1y {
            scale[0] = -scale[0];
        } else {
            scale[1] = -scale[1];
        }
    }
    if scale[0] != 0.0 {
        r0x /= scale[0];
        r0y /= scale[0];
    }
    if scale[1] != 0.0 {
        r1x /= scale[1];
        r1y /= scale[1];
    }
    let angle = r0y.atan2(r0x);
    if angle != 0.0 {
        // Turn the rows back by the angle.
        let (sn, cs) = (-r0y, r0x);
        let (m11, m12, m21, m22) = (r0x, r0y, r1x, r1y);
        r0x = cs * m11 + sn * m21;
        r0y = cs * m12 + sn * m22;
        r1x = -sn * m11 + cs * m21;
        r1y = -sn * m12 + cs * m22;
    }
    Some(Plane { translate: [m[12], m[13]], scale, angle: angle.to_degrees(), rest: [r0x, r0y, r1x, r1y] })
}

fn recompose_plane(p: &Plane) -> Mat {
    let (s, c) = p.angle.to_radians().sin_cos();
    let [a, b, cc, d] = p.rest;
    // rotation · remainder, then each row scaled.
    let r0 = [c * a + s * cc, c * b + s * d];
    let r1 = [-s * a + c * cc, -s * b + c * d];
    let mut m = IDENTITY;
    m[0] = p.scale[0] * r0[0];
    m[1] = p.scale[0] * r0[1];
    m[4] = p.scale[1] * r1[0];
    m[5] = p.scale[1] * r1[1];
    m[12] = p.translate[0];
    m[13] = p.translate[1];
    m
}

/// Two plane transforms `f` of the way between, as the CSS Transforms
/// specification interpolates decomposed 2-D matrices (which is what Core
/// Animation's interpolation of affine transforms measures as): an axis
/// flipped in one and the other axis in the other become a half turn, and
/// the angle goes the shorter way, 0 counting as a whole turn.
fn interpolate_plane(a: &Plane, b: &Plane, f: f64) -> Mat {
    let (mut a, mut b) = (*a, *b);
    if (a.scale[0] < 0.0 && b.scale[1] < 0.0) || (a.scale[1] < 0.0 && b.scale[0] < 0.0) {
        a.scale = [-a.scale[0], -a.scale[1]];
        a.angle += if a.angle < 0.0 { 180.0 } else { -180.0 };
    }
    if a.angle == 0.0 {
        a.angle = 360.0;
    }
    if b.angle == 0.0 {
        b.angle = 360.0;
    }
    if (a.angle - b.angle).abs() > 180.0 {
        if a.angle > b.angle {
            a.angle -= 360.0;
        } else {
            b.angle -= 360.0;
        }
    }
    let l = |x: f64, y: f64| x + (y - x) * f;
    recompose_plane(&Plane {
        translate: [l(a.translate[0], b.translate[0]), l(a.translate[1], b.translate[1])],
        scale: [l(a.scale[0], b.scale[0]), l(a.scale[1], b.scale[1])],
        angle: l(a.angle, b.angle),
        rest: std::array::from_fn(|i| l(a.rest[i], b.rest[i])),
    })
}

/// Interpolate two transforms `f` of the way from `a` to `b`, taking them
/// apart: two affine transforms as plane transforms (see
/// [`interpolate_plane`]), others in three dimensions, spherically
/// interpolating the rotation (so a quarter turn turns rather than
/// shrinking through the middle); componentwise where either can't be
/// taken apart.
pub(crate) fn interpolate(a: &Mat, b: &Mat, f: f64) -> Mat {
    if is_affine(a)
        && is_affine(b)
        && let (Some(pa), Some(pb)) = (decompose_plane(a), decompose_plane(b))
    {
        return interpolate_plane(&pa, &pb, f);
    }
    match (decompose(a), decompose(b)) {
        (Some(pa), Some(pb)) => {
            let lerp3 = |x: [f64; 3], y: [f64; 3]| [0, 1, 2].map(|i| x[i] + (y[i] - x[i]) * f);
            let lerp4 = |x: [f64; 4], y: [f64; 4]| [0, 1, 2, 3].map(|i| x[i] + (y[i] - x[i]) * f);
            recompose(&Parts {
                translate: lerp3(pa.translate, pb.translate),
                scale: lerp3(pa.scale, pb.scale),
                skew: lerp3(pa.skew, pb.skew),
                quaternion: slerp(pa.quaternion, pb.quaternion, f),
                perspective: lerp4(pa.perspective, pb.perspective),
            })
        }
        _ => std::array::from_fn(|i| a[i] + (b[i] - a[i]) * f),
    }
}

fn slerp(a: [f64; 4], b: [f64; 4], f: f64) -> [f64; 4] {
    let mut d: f64 = (0..4).map(|i| a[i] * b[i]).sum();
    let mut b = b;
    if d < 0.0 {
        b = b.map(|v| -v);
        d = -d;
    }
    if d > 0.9995 {
        let q: [f64; 4] = std::array::from_fn(|i| a[i] + (b[i] - a[i]) * f);
        let n = q.iter().map(|v| v * v).sum::<f64>().sqrt();
        return q.map(|v| v / n);
    }
    let theta = d.clamp(-1.0, 1.0).acos();
    let s = theta.sin();
    let wa = ((1.0 - f) * theta).sin() / s;
    let wb = (f * theta).sin() / s;
    std::array::from_fn(|i| a[i] * wa + b[i] * wb)
}

/// A transform's parts as Core Animation's key paths name them
/// (`transform.scale.x`, `transform.rotation.z`, …): translation, scale
/// and the rotation as angles about x, y and z (applied in that order).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Components {
    pub translate: [f64; 3],
    pub scale: [f64; 3],
    pub rotate: [f64; 3],
}

impl Components {
    pub fn of(m: &Mat) -> Components {
        let translate = [m[12], m[13], m[14]];
        let mut rows = [[m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]];
        let mut scale = rows.map(length3);
        if dot3(rows[0], cross3(rows[1], rows[2])) < 0.0 {
            scale[0] = -scale[0];
        }
        for i in 0..3 {
            if scale[i] != 0.0 {
                rows[i] = rows[i].map(|v| v / scale[i]);
            }
        }
        // rows = Rx · Ry · Rz (row vectors: x first), whose third column
        // and first row give the angles.
        let sy = (-rows[0][2]).clamp(-1.0, 1.0);
        let ry = sy.asin();
        let (rx, rz) = if sy.abs() < 0.999_999 {
            (rows[1][2].atan2(rows[2][2]), rows[0][1].atan2(rows[0][0]))
        } else {
            (0.0, (-rows[1][0]).atan2(rows[1][1]))
        };
        Components { translate, scale, rotate: [rx, ry, rz] }
    }

    pub fn matrix(&self) -> Mat {
        let s = scale(self.scale[0], self.scale[1], self.scale[2]);
        let r = concat(
            &concat(&rotation(self.rotate[0], 1.0, 0.0, 0.0), &rotation(self.rotate[1], 0.0, 1.0, 0.0)),
            &rotation(self.rotate[2], 0.0, 0.0, 1.0),
        );
        let t = translation(self.translate[0], self.translate[1], self.translate[2]);
        concat(&concat(&s, &r), &t)
    }
}

// Timing curves.

/// A cubic Bézier timing curve from (0, 0) to (1, 1) through two control
/// points, as `CAMediaTimingFunction` keeps it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bezier {
    pub c1: [f64; 2],
    pub c2: [f64; 2],
}

impl Bezier {
    pub const LINEAR: Bezier = Bezier { c1: [0.0, 0.0], c2: [1.0, 1.0] };

    fn coefficients(p1: f64, p2: f64) -> (f64, f64, f64) {
        let c = 3.0 * p1;
        let b = 3.0 * (p2 - p1) - c;
        let a = 1.0 - c - b;
        (a, b, c)
    }

    fn sample(p1: f64, p2: f64, s: f64) -> f64 {
        let (a, b, c) = Self::coefficients(p1, p2);
        ((a * s + b) * s + c) * s
    }

    fn slope(p1: f64, p2: f64, s: f64) -> f64 {
        let (a, b, c) = Self::coefficients(p1, p2);
        (3.0 * a * s + 2.0 * b) * s + c
    }

    /// The curve's y where its x is `x` (0 to 1; clamped).
    pub fn value(&self, x: f64) -> f64 {
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        let (x1, x2) = (self.c1[0], self.c2[0]);
        // Newton's method, then bisection where it stalls.
        let mut s = x;
        for _ in 0..8 {
            let err = Self::sample(x1, x2, s) - x;
            if err.abs() < 1e-9 {
                return Self::sample(self.c1[1], self.c2[1], s);
            }
            let d = Self::slope(x1, x2, s);
            if d.abs() < 1e-9 {
                break;
            }
            s -= err / d;
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        s = x;
        for _ in 0..64 {
            let v = Self::sample(x1, x2, s);
            if (v - x).abs() < 1e-9 {
                break;
            }
            if v < x {
                lo = s;
            } else {
                hi = s;
            }
            s = 0.5 * (lo + hi);
        }
        Self::sample(self.c1[1], self.c2[1], s)
    }
}

// Springs.

/// A damped spring from rest one unit short of its target, as
/// `CASpringAnimation` moves: mass, stiffness, damping and the initial
/// velocity toward the target (in units of the distance per second).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Spring {
    pub mass: f64,
    pub stiffness: f64,
    pub damping: f64,
    pub velocity: f64,
    /// `allowsOverdamping`: without it, damping beyond critical is taken as
    /// critical (as measured on macOS).
    pub overdamping: bool,
}

impl Spring {
    fn omega(&self) -> f64 {
        (self.stiffness / self.mass).sqrt()
    }

    fn zeta(&self) -> f64 {
        let z = self.damping / (2.0 * (self.stiffness * self.mass).sqrt());
        if self.overdamping { z } else { z.min(1.0) }
    }

    /// Where the spring is after `t` seconds, as the fraction of the way
    /// (1 at the target; it overshoots when underdamped).
    pub fn value(&self, t: f64) -> f64 {
        1.0 + self.offset(t)
    }

    /// Its displacement from the target: -1 at the start.
    fn offset(&self, t: f64) -> f64 {
        let (w0, z) = (self.omega(), self.zeta());
        let (x0, v0) = (-1.0, self.velocity);
        if z < 1.0 {
            let wd = w0 * (1.0 - z * z).sqrt();
            let b = (z * w0 * x0 + v0) / wd;
            (-z * w0 * t).exp() * (x0 * (wd * t).cos() + b * (wd * t).sin())
        } else if z == 1.0 {
            (-w0 * t).exp() * (x0 + (v0 + w0 * x0) * t)
        } else {
            // Overdamped: two decaying exponentials.
            let r = w0 * (z * z - 1.0).sqrt();
            let (s1, s2) = (-z * w0 + r, -z * w0 - r);
            let c2 = (v0 - s1 * x0) / (s2 - s1);
            let c1 = x0 - c2;
            c1 * (s1 * t).exp() + c2 * (s2 * t).exp()
        }
    }

    /// `settlingDuration`, as macOS works it out (measured): an
    /// underdamped spring settles when its envelope, the sum of its
    /// oscillation's amplitudes, is within a thousandth of the distance;
    /// a critically damped one at the first tenth of a second (counted by
    /// adding tenths) where it is; one without damping never does.
    pub fn settling_duration(&self) -> f64 {
        let (w0, z) = (self.omega(), self.zeta());
        if z <= 0.0 {
            return f32::MAX as f64;
        }
        if z < 1.0 {
            let wd = w0 * (1.0 - z * z).sqrt();
            let (x0, v0) = (-1.0f64, self.velocity);
            let b = (z * w0 * x0 + v0) / wd;
            let envelope = x0.abs() + b.abs();
            return ((1000.0f64).ln() + envelope.ln()) / (z * w0);
        }
        let mut t = 0.0;
        for _ in 0..100_000 {
            if self.offset(t).abs() < 0.001 {
                return t;
            }
            t += 0.1;
        }
        t
    }
}

// The media clock.

/// `CACurrentMediaTime()`: seconds of the monotonic clock (the time since
/// boot, as `mach_absolute_time` counts on macOS), from any thread. It is
/// the clock `-[NSProcessInfo systemUptime]` reads, so the two share a
/// timebase as they do on macOS.
pub(crate) fn media_now() -> f64 {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol, "{a} is not {b}");
    }

    #[test]
    fn named_curves_match_macos() {
        // Values measured through presentation layers on macOS.
        let ease_in = Bezier { c1: [0.42, 0.0], c2: [1.0, 1.0] };
        let ease_out = Bezier { c1: [0.0, 0.0], c2: [0.58, 1.0] };
        let both = Bezier { c1: [0.42, 0.0], c2: [0.58, 1.0] };
        let default = Bezier { c1: [0.25, 0.1], c2: [0.25, 1.0] };
        for (b, want) in [
            (ease_in, [0.017026613, 0.09346466, 0.31535682, 0.6218605, 0.83942777]),
            (ease_out, [0.16057223, 0.37813953, 0.6846432, 0.9065353, 0.9829734]),
            (both, [0.019722449, 0.12916191, 0.5, 0.8708381, 0.98027754]),
            (default, [0.094795, 0.4085106, 0.8024034, 0.96045905, 0.99431646]),
        ] {
            for (x, y) in [0.1, 0.25, 0.5, 0.75, 0.9].iter().zip(want) {
                close(b.value(*x), y, 1e-4);
            }
        }
        close(Bezier::LINEAR.value(0.3), 0.3, 1e-6);
    }

    #[test]
    fn springs_match_macos() {
        let s = |m, k, c, v| Spring { mass: m, stiffness: k, damping: c, velocity: v, overdamping: false };
        // settlingDuration, measured.
        close(s(1.0, 100.0, 10.0, 0.0).settling_duration(), 1.4727003346780927, 1e-12);
        close(s(1.0, 300.0, 20.0, 0.0).settling_duration(), 0.7442555275721707, 1e-12);
        close(s(2.0, 100.0, 5.0, 0.0).settling_duration(), 5.658348137358159, 1e-12);
        close(s(1.0, 100.0, 10.0, 5.0).settling_duration(), 1.3815510557964275, 1e-12);
        close(s(1.0, 100.0, 10.0, -5.0).settling_duration(), 1.5350814063145797, 1e-12);
        close(s(1.0, 100.0, 20.0, 0.0).settling_duration(), 0.9999999999999999, 1e-15);
        close(s(1.0, 25.0, 10.0, 0.0).settling_duration(), 1.9000000000000006, 1e-15);
        close(s(1.0, 1.0, 2.0, 0.0).settling_duration(), 9.299999999999983, 1e-12);
        assert_eq!(s(1.0, 100.0, 0.0, 0.0).settling_duration(), f32::MAX as f64);
        // Positions, measured (from 0 to 100; macOS's are single
        // precision in places).
        close(s(1.0, 100.0, 10.0, 0.0).value(0.05) * 100.0, 10.44054701924324, 1e-4);
        close(s(1.0, 100.0, 20.0, 0.0).value(0.05) * 100.0, 9.020400792360306, 1e-4);
        close(s(1.0, 100.0, 30.0, 0.0).value(0.05) * 100.0, 9.020400792360306, 1e-4);
        close(s(1.0, 100.0, 10.0, 5.0).value(0.05) * 100.0, 29.307806491851807, 1e-4);
        close(s(1.0, 100.0, 10.0, -5.0).value(0.05) * 100.0, -8.426713198423386, 1e-4);
    }

    #[test]
    fn transforms_compose_as_core_animation_does() {
        let t = scale(2.0, 3.0, 4.0);
        let r = rotation(0.5, 0.0, 0.0, 1.0);
        let rs = concat(&r, &t);
        close(rs[0], 1.7551651237807455, 1e-12);
        close(rs[1], 1.438276615812609, 1e-12);
        let inv = invert(&concat(&t, &translation(1.0, 2.0, 3.0))).expect("invertible");
        close(inv[12], -0.5, 1e-12);
        close(inv[13], -0.6666666666666666, 1e-12);
        assert!(invert(&scale(0.0, 1.0, 1.0)).is_none());
        assert!(is_affine(&r) && !is_affine(&t));
        let rx = rotation(0.5, 2.0, 0.0, 0.0);
        close(rx[6], 0.479425538604203, 1e-12);
        close(rx[9], -0.479425538604203, 1e-12);
    }

    #[test]
    fn components_round_trip() {
        let c = Components { translate: [3.0, 4.0, 0.0], scale: [2.0, 2.0, 2.0], rotate: [0.0, 0.0, 0.5] };
        let m = c.matrix();
        let back = Components::of(&m);
        for i in 0..3 {
            close(back.translate[i], c.translate[i], 1e-9);
            close(back.scale[i], c.scale[i], 1e-9);
            close(back.rotate[i], c.rotate[i], 1e-9);
        }
        // Scale then rotation, as setting `transform.rotation.z` on a scaled
        // transform does on macOS.
        close(m[0], 1.7551651237807455, 1e-12);
        close(m[1], 0.958851077208406, 1e-12);
    }

    #[test]
    fn interpolating_turns_rotations() {
        let a = IDENTITY;
        let b = rotation(std::f64::consts::FRAC_PI_2, 0.0, 0.0, 1.0);
        let mid = interpolate(&a, &b, 0.5);
        let want = rotation(std::f64::consts::FRAC_PI_4, 0.0, 0.0, 1.0);
        for i in 0..16 {
            close(mid[i], want[i], 1e-9);
        }
        let s = interpolate(&scale(1.0, 1.0, 1.0), &scale(3.0, 3.0, 1.0), 0.5);
        close(s[0], 2.0, 1e-12);
        let p = decompose(&concat(&scale(2.0, 3.0, 1.0), &translation(5.0, 6.0, 0.0))).expect("parts");
        assert_eq!(p.translate, [5.0, 6.0, 0.0]);
        let back = recompose(&p);
        close(back[0], 2.0, 1e-12);
        close(back[13], 6.0, 1e-12);
    }

    #[test]
    fn plane_transforms_interpolate_as_macos() {
        // Measured on macOS: from the identity (or the first) to the
        // second, (m11, m12, m21, m22) at a quarter and at half.
        let pi = std::f64::consts::PI;
        // A half turn's eighth: cos and sin of 45°.
        const H: f64 = std::f64::consts::FRAC_1_SQRT_2;
        let affine = |a, b, c, d| {
            let mut m = IDENTITY;
            m[0] = a;
            m[1] = b;
            m[4] = c;
            m[5] = d;
            m
        };
        for (from, to, quarter, half) in [
            (IDENTITY, scale(-1.0, 1.0, 1.0), [0.5, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]),
            (IDENTITY, scale(1.0, -1.0, 1.0), [1.0, 0.0, 0.0, 0.5], [1.0, 0.0, 0.0, 0.0]),
            (IDENTITY, scale(-1.0, -1.0, 1.0), [H, -H, H, H], [0.0, -1.0, 1.0, 0.0]),
            (IDENTITY, rotation(pi, 0.0, 0.0, 1.0), [H, -H, H, H], [0.0, -1.0, 1.0, 0.0]),
            (IDENTITY, affine(1.0, 0.0, 0.5, 1.0), [1.0, 0.0, 0.1151, 1.0023], [1.0, 0.0, 0.2368, 1.0031]),
            (
                IDENTITY,
                concat(&rotation(0.5, 0.0, 0.0, 1.0), &scale(-1.0, 1.0, 1.0)),
                [0.4961, -0.0623, 0.1247, 0.9922],
                [0.0, 0.0, 0.2474, 0.9689],
            ),
            (
                IDENTITY,
                concat(&scale(2.0, 1.0, 1.0), &rotation(1.5, 0.0, 0.0, 1.0)),
                [1.1631, 0.4578, -0.3663, 0.9305],
                [1.0975, 1.0225, -0.6816, 0.7317],
            ),
            (scale(-1.0, 1.0, 1.0), scale(1.0, -1.0, 1.0), [-H, -H, -H, H], [0.0, -1.0, -1.0, 0.0]),
        ] {
            for (f, want) in [(0.25, quarter), (0.5, half)] {
                let m = interpolate(&from, &to, f);
                for (got, w) in [m[0], m[1], m[4], m[5]].iter().zip(want) {
                    close(*got, w, 2e-4);
                }
            }
        }
    }
}
