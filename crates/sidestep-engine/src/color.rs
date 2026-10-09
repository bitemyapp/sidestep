//! sRGB's transfer curve, and grays weighed as the eye weighs them.

/// The luminance of sRGB `rgb`, in linear light.
pub fn luminance(rgb: [f64; 3]) -> f64 {
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}

/// The gray of sRGB `rgb`: its luminance, weighed in linear light, encoded
/// again as sRGB encodes.
pub fn gray_of(rgb: [f64; 3]) -> f64 {
    encoded(luminance(rgb))
}

/// sRGB's transfer curve, undone; odd, so extended values keep their sign.
pub fn linear(v: f64) -> f64 {
    let a = v.abs();
    let l = if a <= 0.04045 { a / 12.92 } else { ((a + 0.055) / 1.055).powf(2.4) };
    l.copysign(v)
}

/// sRGB's transfer curve; odd, so extended values keep their sign.
pub fn encoded(v: f64) -> f64 {
    let a = v.abs();
    let e = if a <= 0.003_130_8 { a * 12.92 } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
    e.copysign(v)
}
