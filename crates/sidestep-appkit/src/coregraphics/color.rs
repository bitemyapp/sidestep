//! `CGColorSpace` and `CGColor`.
//!
//! A color space is its model, how many components a color in it has, and
//! how those become sRGB for drawing. Colors are managed by formula, as
//! `NSColor`'s are (`crate::color`), not by profiles: RGB spaces are sRGB
//! but for linear ones (encoded with sRGB's curve), Display P3 (converted
//! through its matrix) and Generic RGB (its gamma of 1.8 and the matrix
//! macOS converts it with); Generic Gray has that gamma too, other grays
//! are taken as they are; CMYK subtracts from white; Lab and XYZ go
//! through the D50 reference white. Image samples in Generic RGB or gray
//! are drawn as they are ([`CGColorSpaceImpl::samples_to_srgb`]): AppKit's
//! bitmaps in calibrated spaces hold what was drawn into them unconverted,
//! and their CGImages are in those spaces. The named spaces are shared
//! instances, one per name, as CoreGraphics' are; the device spaces are
//! named spaces too (`kCGColorSpaceDeviceRGB`, …), as CoreGraphics names
//! them.
//!
//! A color is immutable: its space and its components, alpha last, clamped
//! to 0…1 unless the space is extended (as CoreGraphics clamps them).

use std::ffi::{c_float, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::{CFData, CFDictionary, CFString, CFType, CFTypeID, CGAffineTransform, CGFloat, CGRect};
use objc2_core_graphics::{
    CGColor, CGColorRenderingIntent, CGColorSpace, CGColorSpaceModel, CGDataProvider, CGPattern, CGPatternCallbacks,
    CGPatternTiling,
};
use objc2_foundation::{NSString, NSUInteger};

use crate::color::Space;
use crate::protocol::Color;

// The names of the spaces and colors, with CoreGraphics' values (the
// constants' own names, but for the old names of the HDR spaces, which
// name the new ones).
sidestep_foundation::constant_string!(kCGColorSpaceGenericGray = "kCGColorSpaceGenericGray");
sidestep_foundation::constant_string!(kCGColorSpaceGenericRGB = "kCGColorSpaceGenericRGB");
sidestep_foundation::constant_string!(kCGColorSpaceGenericCMYK = "kCGColorSpaceGenericCMYK");
sidestep_foundation::constant_string!(kCGColorSpaceDisplayP3 = "kCGColorSpaceDisplayP3");
sidestep_foundation::constant_string!(kCGColorSpaceGenericRGBLinear = "kCGColorSpaceGenericRGBLinear");
sidestep_foundation::constant_string!(kCGColorSpaceAdobeRGB1998 = "kCGColorSpaceAdobeRGB1998");
sidestep_foundation::constant_string!(kCGColorSpaceSRGB = "kCGColorSpaceSRGB");
sidestep_foundation::constant_string!(kCGColorSpaceGenericGrayGamma2_2 = "kCGColorSpaceGenericGrayGamma2_2");
sidestep_foundation::constant_string!(kCGColorSpaceGenericXYZ = "kCGColorSpaceGenericXYZ");
sidestep_foundation::constant_string!(kCGColorSpaceGenericLab = "kCGColorSpaceGenericLab");
sidestep_foundation::constant_string!(kCGColorSpaceACESCGLinear = "kCGColorSpaceACESCGLinear");
sidestep_foundation::constant_string!(kCGColorSpaceITUR_709 = "kCGColorSpaceITUR_709");
sidestep_foundation::constant_string!(kCGColorSpaceITUR_709_PQ = "kCGColorSpaceITUR_709_PQ");
sidestep_foundation::constant_string!(kCGColorSpaceITUR_709_HLG = "kCGColorSpaceITUR_709_HLG");
sidestep_foundation::constant_string!(kCGColorSpaceITUR_2020 = "kCGColorSpaceITUR_2020");
sidestep_foundation::constant_string!(kCGColorSpaceITUR_2020_sRGBGamma = "kCGColorSpaceITUR_2020_sRGBGamma");
sidestep_foundation::constant_string!(kCGColorSpaceROMMRGB = "kCGColorSpaceROMMRGB");
sidestep_foundation::constant_string!(kCGColorSpaceDCIP3 = "kCGColorSpaceDCIP3");
sidestep_foundation::constant_string!(kCGColorSpaceLinearITUR_2020 = "kCGColorSpaceLinearITUR_2020");
sidestep_foundation::constant_string!(kCGColorSpaceExtendedITUR_2020 = "kCGColorSpaceExtendedITUR_2020");
sidestep_foundation::constant_string!(kCGColorSpaceExtendedLinearITUR_2020 = "kCGColorSpaceExtendedLinearITUR_2020");
sidestep_foundation::constant_string!(kCGColorSpaceLinearDisplayP3 = "kCGColorSpaceLinearDisplayP3");
sidestep_foundation::constant_string!(kCGColorSpaceExtendedDisplayP3 = "kCGColorSpaceExtendedDisplayP3");
sidestep_foundation::constant_string!(kCGColorSpaceExtendedLinearDisplayP3 = "kCGColorSpaceExtendedLinearDisplayP3");
sidestep_foundation::constant_string!(kCGColorSpaceITUR_2100_PQ = "kCGColorSpaceITUR_2100_PQ");
sidestep_foundation::constant_string!(kCGColorSpaceITUR_2100_HLG = "kCGColorSpaceITUR_2100_HLG");
sidestep_foundation::constant_string!(kCGColorSpaceDisplayP3_PQ = "kCGColorSpaceDisplayP3_PQ");
sidestep_foundation::constant_string!(kCGColorSpaceDisplayP3_HLG = "kCGColorSpaceDisplayP3_HLG");
sidestep_foundation::constant_string!(kCGColorSpaceITUR_2020_PQ = "kCGColorSpaceITUR_2100_PQ");
sidestep_foundation::constant_string!(kCGColorSpaceITUR_2020_HLG = "kCGColorSpaceITUR_2100_HLG");
sidestep_foundation::constant_string!(kCGColorSpaceDisplayP3_PQ_EOTF = "kCGColorSpaceDisplayP3_PQ");
sidestep_foundation::constant_string!(kCGColorSpaceITUR_2020_PQ_EOTF = "kCGColorSpaceITUR_2100_PQ");
sidestep_foundation::constant_string!(kCGColorSpaceExtendedSRGB = "kCGColorSpaceExtendedSRGB");
sidestep_foundation::constant_string!(kCGColorSpaceLinearSRGB = "kCGColorSpaceLinearSRGB");
sidestep_foundation::constant_string!(kCGColorSpaceExtendedLinearSRGB = "kCGColorSpaceExtendedLinearSRGB");
sidestep_foundation::constant_string!(kCGColorSpaceExtendedGray = "kCGColorSpaceExtendedGray");
sidestep_foundation::constant_string!(kCGColorSpaceLinearGray = "kCGColorSpaceLinearGray");
sidestep_foundation::constant_string!(kCGColorSpaceExtendedLinearGray = "kCGColorSpaceExtendedLinearGray");
sidestep_foundation::constant_string!(kCGColorSpaceCoreMedia709 = "kCGColorSpaceCoreMedia709");
sidestep_foundation::constant_string!(kCGColorSpaceExtendedRange = "kCGColorSpaceExtendedRange");
sidestep_foundation::constant_string!(kCGColorWhite = "kCGColorWhite");
sidestep_foundation::constant_string!(kCGColorBlack = "kCGColorBlack");
sidestep_foundation::constant_string!(kCGColorClear = "kCGColorClear");

/// What a space's components are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Model {
    Gray,
    Rgb,
    Cmyk,
    Lab,
    Xyz,
    Indexed,
    Pattern,
}

impl Model {
    fn cg(self) -> CGColorSpaceModel {
        match self {
            Model::Gray => CGColorSpaceModel::Monochrome,
            Model::Rgb => CGColorSpaceModel::RGB,
            Model::Cmyk => CGColorSpaceModel::CMYK,
            Model::Lab => CGColorSpaceModel::Lab,
            Model::Xyz => CGColorSpaceModel::XYZ,
            Model::Indexed => CGColorSpaceModel::Indexed,
            Model::Pattern => CGColorSpaceModel::Pattern,
        }
    }
}

/// How a space's color components become sRGB's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Curve {
    /// Taken as sRGB's (or as gray levels).
    Srgb,
    /// Linear light, encoded with sRGB's curve.
    Linear,
    DisplayP3,
    LinearDisplayP3,
    /// Generic RGB's primaries and a gamma of 1.8 (Generic Gray: the
    /// gamma alone).
    Generic,
    /// Generic RGB's primaries in linear light.
    GenericLinear,
}

/// A named space: its name, model, curve, whether it's extended, and the
/// `NSColorSpace` it is, if one.
struct Named(&'static str, Model, Curve, bool, Option<Space>);

const NAMED: &[Named] = &[
    Named("kCGColorSpaceSRGB", Model::Rgb, Curve::Srgb, false, Some(Space::Srgb)),
    Named("kCGColorSpaceExtendedSRGB", Model::Rgb, Curve::Srgb, true, Some(Space::ExtendedSrgb)),
    Named("kCGColorSpaceDisplayP3", Model::Rgb, Curve::DisplayP3, false, Some(Space::DisplayP3)),
    Named("kCGColorSpaceAdobeRGB1998", Model::Rgb, Curve::Srgb, false, Some(Space::AdobeRgb)),
    Named("kCGColorSpaceGenericRGB", Model::Rgb, Curve::Generic, false, Some(Space::GenericRgb)),
    Named("kCGColorSpaceDeviceRGB", Model::Rgb, Curve::Srgb, false, Some(Space::DeviceRgb)),
    Named("kCGColorSpaceGenericGray", Model::Gray, Curve::Generic, false, Some(Space::GenericGray)),
    Named("kCGColorSpaceGenericGrayGamma2_2", Model::Gray, Curve::Srgb, false, Some(Space::Gamma22Gray)),
    Named("kCGColorSpaceExtendedGray", Model::Gray, Curve::Srgb, true, Some(Space::ExtendedGamma22Gray)),
    Named("kCGColorSpaceDeviceGray", Model::Gray, Curve::Srgb, false, Some(Space::DeviceGray)),
    Named("kCGColorSpaceGenericCMYK", Model::Cmyk, Curve::Srgb, false, Some(Space::GenericCmyk)),
    Named("kCGColorSpaceDeviceCMYK", Model::Cmyk, Curve::Srgb, false, Some(Space::DeviceCmyk)),
    Named("kCGColorSpaceGenericRGBLinear", Model::Rgb, Curve::GenericLinear, false, None),
    Named("kCGColorSpaceLinearSRGB", Model::Rgb, Curve::Linear, false, None),
    Named("kCGColorSpaceExtendedLinearSRGB", Model::Rgb, Curve::Linear, true, None),
    Named("kCGColorSpaceLinearGray", Model::Gray, Curve::Linear, false, None),
    Named("kCGColorSpaceExtendedLinearGray", Model::Gray, Curve::Linear, true, None),
    Named("kCGColorSpaceLinearDisplayP3", Model::Rgb, Curve::LinearDisplayP3, false, None),
    Named("kCGColorSpaceExtendedDisplayP3", Model::Rgb, Curve::DisplayP3, true, None),
    Named("kCGColorSpaceExtendedLinearDisplayP3", Model::Rgb, Curve::LinearDisplayP3, true, None),
    Named("kCGColorSpaceDisplayP3_PQ", Model::Rgb, Curve::DisplayP3, false, None),
    Named("kCGColorSpaceDisplayP3_HLG", Model::Rgb, Curve::DisplayP3, false, None),
    Named("kCGColorSpaceDCIP3", Model::Rgb, Curve::DisplayP3, false, None),
    Named("kCGColorSpaceGenericXYZ", Model::Xyz, Curve::Linear, true, None),
    Named("kCGColorSpaceGenericLab", Model::Lab, Curve::Linear, false, None),
    Named("kCGColorSpaceACESCGLinear", Model::Rgb, Curve::Linear, false, None),
    Named("kCGColorSpaceITUR_709", Model::Rgb, Curve::Srgb, false, None),
    Named("kCGColorSpaceITUR_709_PQ", Model::Rgb, Curve::Srgb, false, None),
    Named("kCGColorSpaceITUR_709_HLG", Model::Rgb, Curve::Srgb, false, None),
    Named("kCGColorSpaceITUR_2020", Model::Rgb, Curve::Srgb, false, None),
    Named("kCGColorSpaceITUR_2020_sRGBGamma", Model::Rgb, Curve::Srgb, false, None),
    Named("kCGColorSpaceLinearITUR_2020", Model::Rgb, Curve::Linear, false, None),
    Named("kCGColorSpaceExtendedITUR_2020", Model::Rgb, Curve::Srgb, true, None),
    Named("kCGColorSpaceExtendedLinearITUR_2020", Model::Rgb, Curve::Linear, true, None),
    Named("kCGColorSpaceITUR_2100_PQ", Model::Rgb, Curve::Srgb, false, None),
    Named("kCGColorSpaceITUR_2100_HLG", Model::Rgb, Curve::Srgb, false, None),
    Named("kCGColorSpaceROMMRGB", Model::Rgb, Curve::Srgb, false, None),
    Named("kCGColorSpaceCoreMedia709", Model::Rgb, Curve::Srgb, false, None),
];

/// What a space is.
pub(crate) struct SpaceInfo {
    /// The name, for the named spaces.
    pub name: Option<&'static str>,
    pub model: Model,
    /// Color components, alpha not counted.
    pub components: usize,
    pub curve: Curve,
    pub extended: bool,
    /// The `NSColorSpace` kind this is, if one.
    pub ns: Option<Space>,
    /// The base space of a pattern or indexed space.
    pub base: Option<Retained<CGColorSpaceImpl>>,
    /// An indexed space's colors, `components` of the base's each.
    pub table: Vec<u8>,
    /// The profile an ICC-based space was made from.
    pub icc: Option<Vec<u8>>,
}

pub(crate) struct SpaceIvars {
    pub(crate) info: SpaceInfo,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; spaces are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGColorSpace"]
    #[ivars = SpaceIvars]
    pub(crate) struct CGColorSpaceImpl;

    impl CGColorSpaceImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let info = &self.ivars().info;
            let rest = match info.name {
                Some(name) => format!("({name})"),
                None => format!("({:?}; {} components)", info.model, info.components),
            };
            super::description("CGColorSpace", self, &rest)
        }
    }

    unsafe impl NSObjectProtocol for CGColorSpaceImpl {
        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|o| {
                std::ptr::eq(o as *const AnyObject, (self as *const Self).cast())
                    || o.downcast_ref::<CGColorSpaceImpl>().is_some_and(|o| same_space(self, o))
            })
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            let info = &self.ivars().info;
            info.name.map_or(info.components, |n| n.len() * 31 + info.components)
        }
    }
);

/// Whether two spaces are the same: the same object, or the same name.
pub(crate) fn same_space(a: &CGColorSpaceImpl, b: &CGColorSpaceImpl) -> bool {
    std::ptr::eq(a, b) || (a.ivars().info.name.is_some() && a.ivars().info.name == b.ivars().info.name)
}

impl CGColorSpaceImpl {
    pub(crate) fn info(&self) -> &SpaceInfo {
        &self.ivars().info
    }

    /// Components (alpha not counted) as sRGB red, green and blue.
    pub(crate) fn to_srgb(&self, c: &[f64]) -> [f64; 3] {
        self.convert(c, false)
    }

    /// A color's components in this space, alpha after them (opaque if
    /// missing), as straight sRGB RGBA for drawing.
    pub(crate) fn rgba(&self, comps: &[f64]) -> Color {
        let n = self.info().components;
        let [r, g, b] = self.to_srgb(&comps[..n.min(comps.len())]);
        let a = comps.get(n).copied().unwrap_or(1.0);
        [r, g, b, a].map(|v| v.clamp(0.0, 1.0) as f32)
    }

    /// Whether an image's samples in this space are drawn as they are
    /// (RGB and gray taken as sRGB's, or Generic RGB's and gray's).
    pub(crate) fn samples_are_srgb(&self) -> bool {
        let info = self.info();
        matches!(info.model, Model::Rgb | Model::Gray) && matches!(info.curve, Curve::Srgb | Curve::Generic)
    }

    /// An image's samples as sRGB: as [`to_srgb`](Self::to_srgb), but for
    /// Generic RGB and gray, whose samples are drawn as they are (see the
    /// module's documentation).
    pub(crate) fn samples_to_srgb(&self, c: &[f64]) -> [f64; 3] {
        self.convert(c, true)
    }

    fn convert(&self, c: &[f64], samples: bool) -> [f64; 3] {
        let info = self.info();
        let at = |i: usize| c.get(i).copied().unwrap_or(0.0);
        let rgb = match info.model {
            Model::Gray => [at(0); 3],
            Model::Rgb => [at(0), at(1), at(2)],
            Model::Cmyk => {
                let k = 1.0 - at(3);
                return [(1.0 - at(0)) * k, (1.0 - at(1)) * k, (1.0 - at(2)) * k];
            }
            Model::Xyz => return xyz_to_srgb([at(0), at(1), at(2)]),
            Model::Lab => return xyz_to_srgb(lab_to_xyz([at(0), at(1), at(2)])),
            Model::Indexed => {
                let Some(base) = &info.base else { return [0.0; 3] };
                let n = base.info().components;
                let i = at(0).round().max(0.0) as usize;
                let entry: Vec<f64> =
                    (0..n).map(|k| info.table.get(i * n + k).map_or(0.0, |&v| f64::from(v) / 255.0)).collect();
                return base.convert(&entry, samples);
            }
            Model::Pattern => return [0.0; 3],
        };
        match info.curve {
            Curve::Srgb => rgb,
            Curve::Generic if samples => rgb,
            Curve::Linear | Curve::GenericLinear if samples || info.model == Model::Gray => {
                rgb.map(crate::color::encoded)
            }
            Curve::Linear => rgb.map(crate::color::encoded),
            Curve::DisplayP3 => crate::color::p3_to_srgb(rgb),
            Curve::LinearDisplayP3 => crate::color::p3_linear_to_srgb(rgb),
            Curve::Generic if info.model == Model::Gray => rgb.map(|v| crate::color::encoded(crate::color::gamma18(v))),
            Curve::Generic => crate::color::generic_to_srgb(rgb),
            Curve::GenericLinear => crate::color::generic_linear_to_srgb(rgb),
        }
    }

    /// sRGB red, green and blue as this space's components, as nearly as
    /// it has them.
    pub(crate) fn components_for_srgb(&self, rgb: [f64; 3]) -> Vec<f64> {
        let info = self.info();
        if let Some(ns) = info.ns {
            return crate::color::from_srgb(ns, rgb);
        }
        match (info.model, info.curve) {
            (Model::Gray, Curve::Generic) => crate::color::from_srgb(Space::GenericGray, rgb),
            (Model::Rgb, Curve::Generic) => crate::color::from_srgb(Space::GenericRgb, rgb),
            (Model::Rgb, Curve::GenericLinear) => crate::color::srgb_to_generic_linear(rgb).to_vec(),
            (Model::Gray, Curve::Linear | Curve::GenericLinear) => {
                vec![crate::color::linear(crate::color::gray_of(rgb))]
            }
            (Model::Gray, _) => vec![crate::color::gray_of(rgb)],
            (Model::Rgb, Curve::Linear) => rgb.map(crate::color::linear).to_vec(),
            (Model::Rgb, Curve::DisplayP3) => crate::color::srgb_to_p3(rgb).to_vec(),
            (Model::Rgb, Curve::LinearDisplayP3) => crate::color::srgb_to_p3(rgb).map(crate::color::linear).to_vec(),
            (Model::Cmyk, _) => {
                let k = 1.0 - rgb[0].max(rgb[1]).max(rgb[2]);
                let f = |v: f64| if k >= 1.0 { 0.0 } else { (1.0 - v - k) / (1.0 - k) };
                vec![f(rgb[0]), f(rgb[1]), f(rgb[2]), k]
            }
            _ => rgb.to_vec(),
        }
    }
}

/// CIE XYZ (D50, CoreGraphics' reference white) to sRGB.
fn xyz_to_srgb(xyz: [f64; 3]) -> [f64; 3] {
    // Bradford-adapted D50 to linear sRGB (D65).
    const M: [[f64; 3]; 3] = [
        [3.134_051_3, -1.617_385_0, -0.490_632_4],
        [-0.978_795_5, 1.916_254_1, 0.033_454_1],
        [0.071_952_3, -0.228_990_4, 1.405_176_5],
    ];
    [0, 1, 2].map(|i| crate::color::encoded(M[i][0] * xyz[0] + M[i][1] * xyz[1] + M[i][2] * xyz[2]))
}

/// CIE L*a*b* to XYZ, D50.
fn lab_to_xyz([l, a, b]: [f64; 3]) -> [f64; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f64| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f64 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    [0.9642 * inv(fx), inv(fy), 0.8249 * inv(fz)]
}

fn make_space(info: SpaceInfo) -> Retained<CGColorSpaceImpl> {
    let this = CGColorSpaceImpl::alloc().set_ivars(SpaceIvars { info });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// The shared instances of the named spaces, never freed.
fn named_instances() -> &'static [usize] {
    static ALL: OnceLock<Vec<usize>> = OnceLock::new();
    ALL.get_or_init(|| {
        NAMED
            .iter()
            .map(|n| {
                let components = match n.1 {
                    Model::Gray => 1,
                    Model::Cmyk => 4,
                    _ => 3,
                };
                let info = SpaceInfo {
                    name: Some(n.0),
                    model: n.1,
                    components,
                    curve: n.2,
                    extended: n.3,
                    ns: n.4,
                    base: None,
                    table: Vec::new(),
                    icc: None,
                };
                Retained::into_raw(make_space(info)) as usize
            })
            .collect()
    })
}

/// The shared space named `name`, if there's one.
pub(crate) fn named(name: &str) -> Option<Retained<CGColorSpaceImpl>> {
    Some(shared(NAMED.iter().position(|n| n.0 == name)?))
}

/// Where sRGB and the device spaces are in [`NAMED`], to reach them
/// without looking for their names.
pub(crate) const SRGB: usize = 0;
pub(crate) const DEVICE_RGB: usize = 5;
pub(crate) const DEVICE_GRAY: usize = 9;
pub(crate) const DEVICE_CMYK: usize = 11;

/// The shared instance of the named space at `at` in [`NAMED`].
pub(crate) fn shared(at: usize) -> Retained<CGColorSpaceImpl> {
    // SAFETY: the instances are never released; retaining one gives the
    // caller its own reference.
    unsafe { Retained::retain(named_instances()[at] as *mut CGColorSpaceImpl) }.expect("a named space")
}

fn named_or_panic(name: &str) -> Retained<CGColorSpaceImpl> {
    named(name).expect("a space Sidestep names")
}

pub(crate) fn srgb() -> Retained<CGColorSpaceImpl> {
    shared(SRGB)
}

/// The space an `NSColorSpace` kind is.
pub(crate) fn for_ns(space: Space) -> Retained<CGColorSpaceImpl> {
    let name = NAMED.iter().find(|n| n.4 == Some(space)).map_or("kCGColorSpaceSRGB", |n| n.0);
    named_or_panic(name)
}

pub(crate) fn space_imp(s: &CGColorSpace) -> &CGColorSpaceImpl {
    // SAFETY: every CGColorSpace is a CGColorSpaceImpl.
    unsafe { &*(s as *const CGColorSpace).cast::<CGColorSpaceImpl>() }
}

/// The name constant (a static string object) holding `name`.
fn name_string(name: &str) -> Option<NonNull<CFString>> {
    let object: &sidestep_runtime::ObjectRef = match name {
        "kCGColorSpaceGenericGray" => &kCGColorSpaceGenericGray,
        "kCGColorSpaceGenericRGB" => &kCGColorSpaceGenericRGB,
        "kCGColorSpaceGenericCMYK" => &kCGColorSpaceGenericCMYK,
        "kCGColorSpaceDisplayP3" => &kCGColorSpaceDisplayP3,
        "kCGColorSpaceGenericRGBLinear" => &kCGColorSpaceGenericRGBLinear,
        "kCGColorSpaceAdobeRGB1998" => &kCGColorSpaceAdobeRGB1998,
        "kCGColorSpaceSRGB" => &kCGColorSpaceSRGB,
        "kCGColorSpaceGenericGrayGamma2_2" => &kCGColorSpaceGenericGrayGamma2_2,
        "kCGColorSpaceGenericXYZ" => &kCGColorSpaceGenericXYZ,
        "kCGColorSpaceGenericLab" => &kCGColorSpaceGenericLab,
        "kCGColorSpaceACESCGLinear" => &kCGColorSpaceACESCGLinear,
        "kCGColorSpaceITUR_709" => &kCGColorSpaceITUR_709,
        "kCGColorSpaceITUR_709_PQ" => &kCGColorSpaceITUR_709_PQ,
        "kCGColorSpaceITUR_709_HLG" => &kCGColorSpaceITUR_709_HLG,
        "kCGColorSpaceITUR_2020" => &kCGColorSpaceITUR_2020,
        "kCGColorSpaceITUR_2020_sRGBGamma" => &kCGColorSpaceITUR_2020_sRGBGamma,
        "kCGColorSpaceROMMRGB" => &kCGColorSpaceROMMRGB,
        "kCGColorSpaceDCIP3" => &kCGColorSpaceDCIP3,
        "kCGColorSpaceLinearITUR_2020" => &kCGColorSpaceLinearITUR_2020,
        "kCGColorSpaceExtendedITUR_2020" => &kCGColorSpaceExtendedITUR_2020,
        "kCGColorSpaceExtendedLinearITUR_2020" => &kCGColorSpaceExtendedLinearITUR_2020,
        "kCGColorSpaceLinearDisplayP3" => &kCGColorSpaceLinearDisplayP3,
        "kCGColorSpaceExtendedDisplayP3" => &kCGColorSpaceExtendedDisplayP3,
        "kCGColorSpaceExtendedLinearDisplayP3" => &kCGColorSpaceExtendedLinearDisplayP3,
        "kCGColorSpaceITUR_2100_PQ" => &kCGColorSpaceITUR_2100_PQ,
        "kCGColorSpaceITUR_2100_HLG" => &kCGColorSpaceITUR_2100_HLG,
        "kCGColorSpaceDisplayP3_PQ" => &kCGColorSpaceDisplayP3_PQ,
        "kCGColorSpaceDisplayP3_HLG" => &kCGColorSpaceDisplayP3_HLG,
        "kCGColorSpaceExtendedSRGB" => &kCGColorSpaceExtendedSRGB,
        "kCGColorSpaceLinearSRGB" => &kCGColorSpaceLinearSRGB,
        "kCGColorSpaceExtendedLinearSRGB" => &kCGColorSpaceExtendedLinearSRGB,
        "kCGColorSpaceExtendedGray" => &kCGColorSpaceExtendedGray,
        "kCGColorSpaceLinearGray" => &kCGColorSpaceLinearGray,
        "kCGColorSpaceExtendedLinearGray" => &kCGColorSpaceExtendedLinearGray,
        "kCGColorSpaceCoreMedia709" => &kCGColorSpaceCoreMedia709,
        "kCGColorSpaceDeviceRGB" => &_SidestepCGColorSpaceDeviceRGB,
        "kCGColorSpaceDeviceGray" => &_SidestepCGColorSpaceDeviceGray,
        "kCGColorSpaceDeviceCMYK" => &_SidestepCGColorSpaceDeviceCMYK,
        _ => return None,
    };
    // SAFETY: an ObjectRef is a pointer to an immortal object, laid out as
    // one.
    let ptr = unsafe { *(object as *const sidestep_runtime::ObjectRef).cast::<*mut CFString>() };
    NonNull::new(ptr)
}

// The device spaces' names, which CoreGraphics has no constants for.
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepCGColorSpaceDeviceRGB = "kCGColorSpaceDeviceRGB"
);
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepCGColorSpaceDeviceGray = "kCGColorSpaceDeviceGray"
);
sidestep_foundation::constant_string!(
    #[doc(hidden)]
    _SidestepCGColorSpaceDeviceCMYK = "kCGColorSpaceDeviceCMYK"
);

/// The text of a CoreFoundation string.
pub(crate) fn cf_text(s: &CFString) -> String {
    // SAFETY: a CFString is an NSString here.
    unsafe { &*(s as *const CFString).cast::<NSString>() }.to_string()
}

// Color spaces.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_COLOR_SPACE
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreateDeviceGray() -> Option<NonNull<CGColorSpace>> {
    Some(super::owned(shared(DEVICE_GRAY)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreateDeviceRGB() -> Option<NonNull<CGColorSpace>> {
    Some(super::owned(shared(DEVICE_RGB)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreateDeviceCMYK() -> Option<NonNull<CGColorSpace>> {
    Some(super::owned(shared(DEVICE_CMYK)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreateWithName(name: Option<&CFString>) -> Option<NonNull<CGColorSpace>> {
    named(&cf_text(name?)).map(super::owned)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceGetName(space: Option<&CGColorSpace>) -> Option<NonNull<CFString>> {
    name_string(space_imp(space?).info().name?)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCopyName(space: Option<&CGColorSpace>) -> Option<NonNull<CFString>> {
    let name = CGColorSpaceGetName(space)?;
    // SAFETY: a live constant string; retaining it gives the caller a
    // reference (constants are never freed anyway).
    unsafe { objc2::ffi::objc_retain(name.as_ptr().cast()) };
    Some(name)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceGetNumberOfComponents(space: Option<&CGColorSpace>) -> usize {
    space.map_or(0, |s| space_imp(s).info().components)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceGetModel(space: Option<&CGColorSpace>) -> CGColorSpaceModel {
    space.map_or(CGColorSpaceModel::Unknown, |s| space_imp(s).info().model.cg())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceGetBaseColorSpace(space: Option<&CGColorSpace>) -> Option<NonNull<CGColorSpace>> {
    space_imp(space?).info().base.as_deref().map(super::borrowed)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCopyBaseColorSpace(space: &CGColorSpace) -> Option<NonNull<CGColorSpace>> {
    space_imp(space).info().base.clone().map(super::owned)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceGetColorTableCount(space: Option<&CGColorSpace>) -> usize {
    let Some(info) = space.map(|s| space_imp(s).info()) else { return 0 };
    let n = info.base.as_ref().map_or(1, |b| b.info().components.max(1));
    if info.model == Model::Indexed { info.table.len() / n } else { 0 }
}

/// # Safety
///
/// `table` has room for the space's table.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGColorSpaceGetColorTable(space: Option<&CGColorSpace>, table: *mut u8) {
    let Some(info) = space.map(|s| space_imp(s).info()) else { return };
    if table.is_null() || info.model != Model::Indexed {
        return;
    }
    // SAFETY: the caller has room for the table.
    unsafe { std::ptr::copy_nonoverlapping(info.table.as_ptr(), table, info.table.len()) };
}

/// # Safety
///
/// `color_table` holds `(last_index + 1)` entries of the base's components.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGColorSpaceCreateIndexed(
    base_space: Option<&CGColorSpace>,
    last_index: usize,
    color_table: *const u8,
) -> Option<NonNull<CGColorSpace>> {
    let base = space_imp(base_space?);
    if color_table.is_null() || last_index > 255 || matches!(base.info().model, Model::Indexed | Model::Pattern) {
        return None;
    }
    let len = (last_index + 1) * base.info().components;
    // SAFETY: as the caller promises.
    let table = unsafe { std::slice::from_raw_parts(color_table, len) }.to_vec();
    Some(super::owned(make_space(SpaceInfo {
        name: None,
        model: Model::Indexed,
        components: 1,
        curve: Curve::Srgb,
        extended: false,
        ns: None,
        base: Some(base.retain()),
        table,
        icc: None,
    })))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreatePattern(base_space: Option<&CGColorSpace>) -> Option<NonNull<CGColorSpace>> {
    Some(super::owned(make_space(SpaceInfo {
        name: None,
        model: Model::Pattern,
        // A colored pattern's components are its base's.
        components: base_space.map_or(0, |b| space_imp(b).info().components),
        curve: Curve::Srgb,
        extended: false,
        ns: None,
        base: base_space.map(|b| space_imp(b).retain()),
        table: Vec::new(),
        icc: None,
    })))
}

/// A space for an ICC profile: its components' kind, read from the
/// profile's header (color management being none, that's what counts).
fn icc_space(profile: &[u8]) -> Option<Retained<CGColorSpaceImpl>> {
    let kind = profile.get(16..20)?;
    let (model, components) = match kind {
        b"GRAY" => (Model::Gray, 1),
        b"RGB " => (Model::Rgb, 3),
        b"CMYK" => (Model::Cmyk, 4),
        b"Lab " => (Model::Lab, 3),
        b"XYZ " => (Model::Xyz, 3),
        _ => return None,
    };
    Some(make_space(SpaceInfo {
        name: None,
        model,
        components,
        curve: Curve::Srgb,
        extended: false,
        ns: None,
        base: None,
        table: Vec::new(),
        icc: Some(profile.to_vec()),
    }))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreateWithICCData(data: Option<&CFType>) -> Option<NonNull<CGColorSpace>> {
    let data = data?;
    // SAFETY: the data is a CFData (an NSData here).
    let bytes = crate::image_rep::data_bytes(unsafe { &*(data as *const CFType).cast::<AnyObject>() })?;
    icc_space(&bytes).map(super::owned)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreateWithICCProfile(data: Option<&CFData>) -> Option<NonNull<CGColorSpace>> {
    // SAFETY: a CFData is a CFType.
    CGColorSpaceCreateWithICCData(data.map(|d| unsafe { &*(d as *const CFData).cast::<CFType>() }))
}

/// # Safety
///
/// `range` is null or holds two values a component.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGColorSpaceCreateICCBased(
    n_components: usize,
    _range: *const CGFloat,
    profile: Option<&CGDataProvider>,
    alternate: Option<&CGColorSpace>,
) -> Option<NonNull<CGColorSpace>> {
    let bytes = profile.and_then(|p| super::data::provider_imp(p).bytes());
    if let Some(space) = bytes.as_deref().and_then(icc_space)
        && space.info().components == n_components
    {
        return Some(super::owned(space));
    }
    if let Some(alt) = alternate
        && space_imp(alt).info().components == n_components
    {
        return Some(super::owned(space_imp(alt).retain()));
    }
    let name = match n_components {
        1 => "kCGColorSpaceGenericGray",
        3 => "kCGColorSpaceSRGB",
        4 => "kCGColorSpaceGenericCMYK",
        _ => return None,
    };
    named(name).map(super::owned)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCopyICCData(space: Option<&CGColorSpace>) -> Option<NonNull<CFData>> {
    let icc = space_imp(space?).info().icc.as_ref()?;
    crate::image_rep::make_data(icc).map(super::owned)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCopyICCProfile(space: Option<&CGColorSpace>) -> Option<NonNull<CFData>> {
    CGColorSpaceCopyICCData(space)
}

/// What a derived space of a named one is: another named space, one with
/// no name, or none.
#[derive(Clone, Copy)]
enum Derived {
    Named(&'static str),
    Unnamed,
    None,
}

/// The named spaces' linearized, extended and standard-range spaces, as
/// macOS answers `CGColorSpaceCreateLinearized`, `…CreateExtended` and
/// `…CreateCopyWithStandardRange` for them (measured on macOS). A space
/// not listed is its own standard-range space and has no others.
fn derived(name: &'static str) -> (Derived, Derived, Derived) {
    use Derived::{Named as N, None as X, Unnamed as U};
    match name {
        "kCGColorSpaceGenericGray" | "kCGColorSpaceGenericRGB" | "kCGColorSpaceGenericRGBLinear" => (U, U, N(name)),
        "kCGColorSpaceAdobeRGB1998" | "kCGColorSpaceACESCGLinear" | "kCGColorSpaceROMMRGB" => (U, U, N(name)),
        "kCGColorSpaceDCIP3" | "kCGColorSpaceCoreMedia709" => (U, U, N(name)),
        "kCGColorSpaceDisplayP3" => (N("kCGColorSpaceLinearDisplayP3"), N("kCGColorSpaceExtendedDisplayP3"), N(name)),
        "kCGColorSpaceSRGB" => (N("kCGColorSpaceLinearSRGB"), N("kCGColorSpaceExtendedSRGB"), N(name)),
        "kCGColorSpaceGenericGrayGamma2_2" => (N("kCGColorSpaceLinearGray"), N("kCGColorSpaceExtendedGray"), N(name)),
        "kCGColorSpaceITUR_709" => (N("kCGColorSpaceLinearSRGB"), U, N(name)),
        "kCGColorSpaceITUR_2020" => (N("kCGColorSpaceLinearITUR_2020"), N("kCGColorSpaceExtendedITUR_2020"), N(name)),
        "kCGColorSpaceITUR_2020_sRGBGamma" => (N("kCGColorSpaceLinearITUR_2020"), U, N(name)),
        "kCGColorSpaceLinearITUR_2020" => (N(name), N("kCGColorSpaceExtendedLinearITUR_2020"), N(name)),
        "kCGColorSpaceExtendedITUR_2020" => {
            (N("kCGColorSpaceExtendedLinearITUR_2020"), N(name), N("kCGColorSpaceITUR_2020"))
        }
        "kCGColorSpaceExtendedLinearITUR_2020" => (N(name), N(name), N("kCGColorSpaceLinearITUR_2020")),
        "kCGColorSpaceLinearDisplayP3" => (N(name), N("kCGColorSpaceExtendedLinearDisplayP3"), N(name)),
        "kCGColorSpaceExtendedDisplayP3" => {
            (N("kCGColorSpaceExtendedLinearDisplayP3"), N(name), N("kCGColorSpaceDisplayP3"))
        }
        "kCGColorSpaceExtendedLinearDisplayP3" => (N(name), N(name), N("kCGColorSpaceLinearDisplayP3")),
        "kCGColorSpaceITUR_2100_PQ" | "kCGColorSpaceITUR_2100_HLG" => (N("kCGColorSpaceLinearITUR_2020"), X, N(name)),
        "kCGColorSpaceDisplayP3_PQ" | "kCGColorSpaceDisplayP3_HLG" => (N("kCGColorSpaceLinearDisplayP3"), X, N(name)),
        "kCGColorSpaceITUR_709_PQ" | "kCGColorSpaceITUR_709_HLG" => (N("kCGColorSpaceLinearSRGB"), X, N(name)),
        "kCGColorSpaceExtendedSRGB" => (N("kCGColorSpaceExtendedLinearSRGB"), N(name), N("kCGColorSpaceSRGB")),
        "kCGColorSpaceLinearSRGB" => (N(name), N("kCGColorSpaceExtendedLinearSRGB"), N(name)),
        "kCGColorSpaceExtendedLinearSRGB" => (N(name), N(name), N("kCGColorSpaceLinearSRGB")),
        "kCGColorSpaceExtendedGray" => {
            (N("kCGColorSpaceExtendedLinearGray"), N(name), N("kCGColorSpaceGenericGrayGamma2_2"))
        }
        "kCGColorSpaceLinearGray" => (N(name), N("kCGColorSpaceExtendedLinearGray"), N(name)),
        "kCGColorSpaceExtendedLinearGray" => (N(name), N(name), N("kCGColorSpaceLinearGray")),
        _ => (X, X, N(name)),
    }
}

/// The linear form of a curve.
fn linear_of(curve: Curve) -> Curve {
    match curve {
        Curve::DisplayP3 | Curve::LinearDisplayP3 => Curve::LinearDisplayP3,
        Curve::Generic | Curve::GenericLinear => Curve::GenericLinear,
        _ => Curve::Linear,
    }
}

/// `space` made linear and/or extended: a named space where macOS names
/// one, else a space of its own with no name, or none.
fn derive(space: &CGColorSpace, linear: bool, extended: bool) -> Option<NonNull<CGColorSpace>> {
    let this = space_imp(space);
    let info = this.info();
    let name = info.name?;
    let (lin, _, _) = derived(name);
    // Linear first, then extended: a name, or `None` for a space with no
    // name; no space at all for `Derived::None`.
    let step = |d: Derived| match d {
        Derived::Named(n) => Some(Some(n)),
        Derived::Unnamed => Some(None),
        Derived::None => None,
    };
    let mut at = Some(name);
    if linear {
        at = step(lin)?;
    }
    if extended {
        at = match at {
            Some(n) => step(derived(n).1)?,
            None => None,
        };
    }
    match at {
        Some(n) => named(n).map(super::owned),
        None => Some(super::owned(make_space(SpaceInfo {
            name: None,
            model: info.model,
            components: info.components,
            curve: if linear { linear_of(info.curve) } else { info.curve },
            extended: extended || info.extended,
            ns: None,
            base: None,
            table: Vec::new(),
            icc: None,
        }))),
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreateLinearized(space: &CGColorSpace) -> Option<NonNull<CGColorSpace>> {
    derive(space, true, false)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreateExtended(space: &CGColorSpace) -> Option<NonNull<CGColorSpace>> {
    derive(space, false, true)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreateExtendedLinearized(space: &CGColorSpace) -> Option<NonNull<CGColorSpace>> {
    derive(space, true, true)
}

/// The space itself when it has no standard-range counterpart: never null
/// (objc2 declares the result non-null).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceCreateCopyWithStandardRange(s: &CGColorSpace) -> Option<NonNull<CGColorSpace>> {
    let standard = space_imp(s).info().name.and_then(|n| match derived(n).2 {
        Derived::Named(n) => named(n),
        _ => None,
    });
    Some(super::owned(standard.unwrap_or_else(|| space_imp(s).retain())))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceUsesExtendedRange(space: &CGColorSpace) -> bool {
    space_imp(space).info().extended
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceIsWideGamutRGB(param1: &CGColorSpace) -> bool {
    let info = space_imp(param1).info();
    info.model == Model::Rgb
        && (info.extended
            || matches!(info.curve, Curve::DisplayP3 | Curve::LinearDisplayP3)
            || info.name.is_some_and(|n| ["2020", "2100", "Adobe", "ROMM", "ACES"].iter().any(|wide| n.contains(wide))))
}

fn hdr_named(space: &CGColorSpace, what: &str) -> bool {
    space_imp(space).info().name.is_some_and(|n| n.contains(what))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceIsHDR(param1: &CGColorSpace) -> bool {
    hdr_named(param1, "_PQ") || hdr_named(param1, "_HLG")
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceUsesITUR_2100TF(param1: &CGColorSpace) -> bool {
    CGColorSpaceIsHDR(param1)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceIsPQBased(s: &CGColorSpace) -> bool {
    hdr_named(s, "_PQ")
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceIsHLGBased(s: &CGColorSpace) -> bool {
    hdr_named(s, "_HLG")
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorSpaceSupportsOutput(space: &CGColorSpace) -> bool {
    !matches!(space_imp(space).info().model, Model::Indexed | Model::Pattern)
}

// Colors.

pub(crate) struct ColorIvars {
    space: Retained<CGColorSpaceImpl>,
    /// The components, alpha last; `count` of them.
    comps: [CGFloat; 6],
    count: usize,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; colors are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGColor"]
    #[ivars = ColorIvars]
    pub(crate) struct CGColorImpl;

    impl CGColorImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let comps: Vec<String> = self.components().iter().map(|v| format!("{v}")).collect();
            let space: Retained<NSString> = unsafe { msg_send![&*self.ivars().space, description] };
            super::description("CGColor", self, &format!("[{space}] ( {} )", comps.join(" ")))
        }
    }

    unsafe impl NSObjectProtocol for CGColorImpl {
        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<CGColorImpl>()).is_some_and(|o| equal(self, o))
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            self.components().iter().fold(self.ivars().count, |h, v| h.wrapping_mul(31).wrapping_add((v * 65535.0) as i64 as usize))
        }
    }
);

impl CGColorImpl {
    pub(crate) fn components(&self) -> &[CGFloat] {
        &self.ivars().comps[..self.ivars().count]
    }

    pub(crate) fn space(&self) -> &CGColorSpaceImpl {
        &self.ivars().space
    }

    pub(crate) fn alpha(&self) -> CGFloat {
        self.components().last().copied().unwrap_or(1.0)
    }

    /// Straight sRGB RGBA, for drawing.
    pub(crate) fn resolve(&self) -> Color {
        self.space().rgba(self.components())
    }

    pub(crate) fn as_cg(&self) -> &CGColor {
        // SAFETY: CGColorImpl is what CGColor names.
        unsafe { &*(self as *const Self).cast::<CGColor>() }
    }
}

fn equal(a: &CGColorImpl, b: &CGColorImpl) -> bool {
    std::ptr::eq(a, b) || (same_space(a.space(), b.space()) && a.components() == b.components())
}

pub(crate) fn color_imp(c: &CGColor) -> &CGColorImpl {
    // SAFETY: every CGColor is a CGColorImpl.
    unsafe { &*(c as *const CGColor).cast::<CGColorImpl>() }
}

/// A new color in `space`: `comps` are its components, alpha last, clamped
/// unless the space is extended (or measures in other units: Lab, XYZ).
pub(crate) fn new_color(space: Retained<CGColorSpaceImpl>, comps: &[CGFloat]) -> Retained<CGColorImpl> {
    let info = space.info();
    let clamp = !info.extended && !matches!(info.model, Model::Lab | Model::Xyz | Model::Indexed);
    let mut values = [0.0; 6];
    let count = comps.len().min(6);
    for (i, v) in comps.iter().take(count).enumerate() {
        values[i] = if clamp || i + 1 == count { v.clamp(0.0, 1.0) } else { *v };
    }
    if info.model == Model::Indexed && count > 0 {
        values[0] = comps[0].clamp(
            0.0,
            (info.table.len() / info.base.as_ref().map_or(1, |b| b.info().components.max(1))).saturating_sub(1) as f64,
        );
    }
    make_color(space, values, count)
}

fn make_color(space: Retained<CGColorSpaceImpl>, comps: [CGFloat; 6], count: usize) -> Retained<CGColorImpl> {
    let this = CGColorImpl::alloc().set_ivars(ColorIvars { space, comps, count });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A new sRGB color of straight RGBA.
pub(crate) fn srgb_color(c: [f64; 4]) -> Retained<CGColorImpl> {
    new_color(srgb(), &c)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_COLOR
}

/// # Safety
///
/// `components` holds the space's components and alpha.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGColorCreate(
    space: Option<&CGColorSpace>,
    components: *const CGFloat,
) -> Option<NonNull<CGColor>> {
    let space = space_imp(space?);
    if components.is_null() || space.info().model == Model::Pattern {
        return None;
    }
    // SAFETY: as the caller promises.
    let comps = unsafe { std::slice::from_raw_parts(components, space.info().components + 1) };
    Some(super::owned(new_color(space.retain(), comps)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorCreateGenericGray(gray: CGFloat, alpha: CGFloat) -> Option<NonNull<CGColor>> {
    Some(super::owned(new_color(named_or_panic("kCGColorSpaceGenericGray"), &[gray, alpha])))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorCreateGenericRGB(
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
) -> Option<NonNull<CGColor>> {
    Some(super::owned(new_color(named_or_panic("kCGColorSpaceGenericRGB"), &[red, green, blue, alpha])))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorCreateGenericCMYK(
    cyan: CGFloat,
    magenta: CGFloat,
    yellow: CGFloat,
    black: CGFloat,
    alpha: CGFloat,
) -> Option<NonNull<CGColor>> {
    Some(super::owned(new_color(named_or_panic("kCGColorSpaceGenericCMYK"), &[cyan, magenta, yellow, black, alpha])))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorCreateGenericGrayGamma2_2(gray: CGFloat, alpha: CGFloat) -> Option<NonNull<CGColor>> {
    Some(super::owned(new_color(named_or_panic("kCGColorSpaceGenericGrayGamma2_2"), &[gray, alpha])))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorCreateSRGB(
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
) -> Option<NonNull<CGColor>> {
    Some(super::owned(srgb_color([red, green, blue, alpha])))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorCreateWithContentHeadroom(
    _headroom: c_float,
    space: Option<&CGColorSpace>,
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
) -> Option<NonNull<CGColor>> {
    let space = space_imp(space?);
    if space.info().model != Model::Rgb {
        return None;
    }
    Some(super::owned(new_color(space.retain(), &[red, green, blue, alpha])))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorGetContentHeadroom(_color: Option<&CGColor>) -> c_float {
    1.0
}

/// The shared instances of the constant colors: white, black and clear.
fn constants() -> &'static [usize; 3] {
    static ALL: OnceLock<[usize; 3]> = OnceLock::new();
    ALL.get_or_init(|| {
        [[1.0, 1.0], [0.0, 1.0], [0.0, 0.0]]
            .map(|c| Retained::into_raw(new_color(named_or_panic("kCGColorSpaceGenericGrayGamma2_2"), &c)) as usize)
    })
}

/// Opaque black (`kCGColorBlack`), shared.
pub(crate) fn black() -> Retained<CGColorImpl> {
    // SAFETY: the constant colors are never released; retaining one gives
    // the caller its own reference.
    unsafe { Retained::retain(constants()[1] as *mut CGColorImpl) }.expect("black")
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorGetConstantColor(color_name: Option<&CFString>) -> Option<NonNull<CGColor>> {
    let name = color_name?;
    // The constants themselves first, by address; then any string equal
    // to one.
    let is = |constant: &sidestep_runtime::ObjectRef| {
        // SAFETY: an ObjectRef is a pointer to an immortal object, laid
        // out as one.
        let ptr = unsafe { *(constant as *const sidestep_runtime::ObjectRef).cast::<*const CFString>() };
        std::ptr::eq(ptr, name)
    };
    let at = if is(&kCGColorWhite) {
        0
    } else if is(&kCGColorBlack) {
        1
    } else if is(&kCGColorClear) {
        2
    } else {
        match cf_text(name).as_str() {
            "kCGColorWhite" => 0,
            "kCGColorBlack" => 1,
            "kCGColorClear" => 2,
            _ => return None,
        }
    };
    NonNull::new(constants()[at] as *mut CGColor)
}

/// Patterns aren't drawn (a pattern fill would need tiles as paints):
/// colors of them are refused, as CoreGraphics refuses colors it can't
/// make.
///
/// # Safety
///
/// Nothing is read.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGColorCreateWithPattern(
    _space: Option<&CGColorSpace>,
    _pattern: Option<&CGPattern>,
    _components: *const CGFloat,
) -> Option<NonNull<CGColor>> {
    None
}

// Patterns: kept for programs that make them, released when they go; not
// drawn.

pub(crate) struct PatternIvars {
    info: *mut c_void,
    release: Option<unsafe extern "C-unwind" fn(*mut c_void)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a pattern is
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGPattern"]
    #[ivars = PatternIvars]
    pub(crate) struct CGPatternImpl;

    unsafe impl NSObjectProtocol for CGPatternImpl {}
);

impl Drop for CGPatternImpl {
    fn drop(&mut self) {
        if let Some(release) = self.ivars().release {
            // SAFETY: the program's callback, with its info.
            unsafe { release(self.ivars().info) };
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPatternGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_PATTERN
}

/// # Safety
///
/// `callbacks` points at callbacks valid for `info`.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C-unwind" fn CGPatternCreate(
    info: *mut c_void,
    _bounds: CGRect,
    _matrix: CGAffineTransform,
    _x_step: CGFloat,
    _y_step: CGFloat,
    _tiling: CGPatternTiling,
    _is_colored: bool,
    callbacks: *const CGPatternCallbacks,
) -> Option<NonNull<CGPattern>> {
    // SAFETY: as the caller promises.
    let release = (!callbacks.is_null()).then(|| unsafe { (*callbacks).releaseInfo }).flatten();
    let this = CGPatternImpl::alloc().set_ivars(PatternIvars { info, release });
    // SAFETY: NSObject's designated initializer.
    let this: Retained<CGPatternImpl> = unsafe { msg_send![super(this), init] };
    Some(super::owned(this))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorCreateCopy(color: Option<&CGColor>) -> Option<NonNull<CGColor>> {
    // Colors are immutable: a copy is the color.
    Some(super::owned(color_imp(color?).retain()))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorCreateCopyWithAlpha(
    color: Option<&CGColor>,
    alpha: CGFloat,
) -> Option<NonNull<CGColor>> {
    let c = color_imp(color?);
    let mut comps = c.ivars().comps;
    let count = c.ivars().count;
    // Kept as given, beyond 0 to 1 too, as CoreGraphics keeps it (drawing
    // clamps it).
    if count > 0 {
        comps[count - 1] = alpha;
    }
    Some(super::owned(make_color(c.ivars().space.clone(), comps, count)))
}

/// # Safety
///
/// `options` is null or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGColorCreateCopyByMatchingToColorSpace(
    param1: Option<&CGColorSpace>,
    _intent: CGColorRenderingIntent,
    color: Option<&CGColor>,
    _options: Option<&CFDictionary>,
) -> Option<NonNull<CGColor>> {
    let (space, c) = (space_imp(param1?), color_imp(color?));
    if matches!(space.info().model, Model::Pattern | Model::Indexed) {
        return None;
    }
    if same_space(space, c.space()) {
        return Some(super::owned(c.retain()));
    }
    let comps = c.components();
    let rgb = c.space().to_srgb(&comps[..comps.len() - 1]);
    let mut out = space.components_for_srgb(rgb);
    out.push(c.alpha());
    Some(super::owned(new_color(space.retain(), &out)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorEqualToColor(color1: Option<&CGColor>, color2: Option<&CGColor>) -> bool {
    match (color1, color2) {
        (Some(a), Some(b)) => equal(color_imp(a), color_imp(b)),
        (None, None) => true,
        _ => false,
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorGetNumberOfComponents(color: Option<&CGColor>) -> usize {
    color.map_or(0, |c| color_imp(c).ivars().count)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorGetComponents(color: Option<&CGColor>) -> *const CGFloat {
    color.map_or(std::ptr::null(), |c| color_imp(c).ivars().comps.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorGetAlpha(color: Option<&CGColor>) -> CGFloat {
    color.map_or(0.0, |c| color_imp(c).alpha())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorGetColorSpace(color: Option<&CGColor>) -> Option<NonNull<CGColorSpace>> {
    Some(super::borrowed(&*color_imp(color?).ivars().space))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGColorGetPattern(_color: Option<&CGColor>) -> Option<NonNull<CGPattern>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_spaces_are_where_they_say() {
        for (at, name) in [
            (SRGB, "kCGColorSpaceSRGB"),
            (DEVICE_RGB, "kCGColorSpaceDeviceRGB"),
            (DEVICE_GRAY, "kCGColorSpaceDeviceGray"),
            (DEVICE_CMYK, "kCGColorSpaceDeviceCMYK"),
        ] {
            assert_eq!(NAMED[at].0, name);
        }
    }

    #[test]
    fn generic_spaces_convert_as_macos_does() {
        // Measured on macOS: Generic Gray 0.5 fills 146; Generic RGB
        // (0.5, 0.25, 0.75) fills (147, 90, 203); Generic RGB red is
        // (1, 0.149, 0) in sRGB.
        let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let gray = named("kCGColorSpaceGenericGray").unwrap();
        assert_eq!(q(gray.to_srgb(&[0.5])[0]), 146);
        let rgb = named("kCGColorSpaceGenericRGB").unwrap();
        assert_eq!(rgb.to_srgb(&[0.5, 0.25, 0.75]).map(q), [147, 90, 203]);
        let red = rgb.to_srgb(&[1.0, 0.0, 0.0]);
        assert!((red[1] - 0.1491).abs() < 1e-3, "{red:?}");
        // Image samples in Generic RGB are drawn as they are.
        assert_eq!(rgb.samples_to_srgb(&[0.5, 0.25, 0.75]), [0.5, 0.25, 0.75]);
    }

    #[test]
    fn spaces_convert_to_srgb() {
        let lin = named("kCGColorSpaceLinearSRGB").unwrap();
        let [r, ..] = lin.to_srgb(&[0.214_041_1, 0.0, 0.0]);
        assert!((r - 0.5).abs() < 1e-3, "{r}");
        let lab = named("kCGColorSpaceGenericLab").unwrap();
        let white = lab.to_srgb(&[100.0, 0.0, 0.0]);
        assert!(white.iter().all(|v| (v - 1.0).abs() < 0.01), "{white:?}");
    }
}
