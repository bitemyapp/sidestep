//! `NSColor` and `NSColorSpace`.
//!
//! A color is immutable, so it may cross threads, and is one of:
//!
//! - **components** in a color space: RGB, gray or CMYK, plus alpha;
//! - a **catalog** color, one of the system colors (`labelColor`,
//!   `windowBackgroundColor`, …), whose value comes from the palette for
//!   the current drawing appearance each time it's used;
//! - a **dynamic** color, whose provider block AppKit calls with the
//!   current drawing appearance each time it's used (never when it's made);
//! - a **system effect** (`colorWithSystemEffect:`) on any other color
//!   but a pattern, worked out from it each time it's used, since it
//!   depends on the appearance;
//! - a **pattern**, an image tiled.
//!
//! [`resolve`] turns any of them into straight sRGB RGBA for drawing: at
//! `set`, `setFill` and `setStroke`, which put the result in the graphics
//! state, and in the conversions (`colorUsingColorSpace:`). A dynamic color
//! given another alpha with `colorWithAlphaComponent:` stays
//! appearance-dependent, as in AppKit.
//!
//! Components are kept as given, beyond 0 to 1 too (`colorWithRed:…` puts
//! such a color in extended sRGB, as AppKit does); drawing clamps them, and
//! so does converting into a space that isn't extended.
//!
//! There's no color management: device, calibrated and generic RGB are
//! all taken as sRGB, and Display P3 converts to sRGB through its matrix.
//! Gray is RGB's luminance, weighed in linear light and encoded again.
//! Color spaces are shared instances, one per kind.

use std::ptr::NonNull;
use std::sync::OnceLock;

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSAppearance, NSColor, NSColorListName, NSColorName, NSColorSpace, NSColorSpaceModel, NSColorSpaceName,
    NSColorSystemEffect, NSColorType, NSControlTint,
};
use objc2_foundation::{NSArray, NSCopying, NSInteger, NSRect, NSString, NSUInteger, NSZone};

use crate::palette::{self, System};
use crate::protocol::{Blend, Color};

// Color space names, with AppKit's values.
sidestep_foundation::constant_string!(NSCalibratedWhiteColorSpace = "NSCalibratedWhiteColorSpace");
sidestep_foundation::constant_string!(NSCalibratedBlackColorSpace = "NSCalibratedBlackColorSpace");
sidestep_foundation::constant_string!(NSCalibratedRGBColorSpace = "NSCalibratedRGBColorSpace");
sidestep_foundation::constant_string!(NSDeviceWhiteColorSpace = "NSDeviceWhiteColorSpace");
sidestep_foundation::constant_string!(NSDeviceBlackColorSpace = "NSDeviceBlackColorSpace");
sidestep_foundation::constant_string!(NSDeviceRGBColorSpace = "NSDeviceRGBColorSpace");
sidestep_foundation::constant_string!(NSDeviceCMYKColorSpace = "NSDeviceCMYKColorSpace");
sidestep_foundation::constant_string!(NSNamedColorSpace = "NSNamedColorSpace");
sidestep_foundation::constant_string!(NSPatternColorSpace = "NSPatternColorSpace");
sidestep_foundation::constant_string!(NSCustomColorSpace = "NSCustomColorSpace");
sidestep_foundation::constant_string!(NSSystemColorsDidChangeNotification = "NSSystemColorsDidChangeNotification");

sidestep_runtime::static_class!(pub NSCOLORSPACE, NSCOLORSPACE_META = "NSColorSpace", || {
    let _ = NSColorSpaceImpl::class();
});

/// The color spaces there are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Space {
    Srgb,
    ExtendedSrgb,
    DisplayP3,
    AdobeRgb,
    GenericRgb,
    DeviceRgb,
    GenericGray,
    Gamma22Gray,
    ExtendedGamma22Gray,
    DeviceGray,
    GenericCmyk,
    DeviceCmyk,
}

const SPACES: [Space; 12] = [
    Space::Srgb,
    Space::ExtendedSrgb,
    Space::DisplayP3,
    Space::AdobeRgb,
    Space::GenericRgb,
    Space::DeviceRgb,
    Space::GenericGray,
    Space::Gamma22Gray,
    Space::ExtendedGamma22Gray,
    Space::DeviceGray,
    Space::GenericCmyk,
    Space::DeviceCmyk,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Model {
    Gray,
    Rgb,
    Cmyk,
}

impl Space {
    fn model(self) -> Model {
        match self {
            Space::GenericGray | Space::Gamma22Gray | Space::ExtendedGamma22Gray | Space::DeviceGray => Model::Gray,
            Space::GenericCmyk | Space::DeviceCmyk => Model::Cmyk,
            _ => Model::Rgb,
        }
    }

    /// Components, alpha not counted.
    fn components(self) -> usize {
        match self.model() {
            Model::Gray => 1,
            Model::Rgb => 3,
            Model::Cmyk => 4,
        }
    }

    fn localized_name(self) -> &'static str {
        match self {
            Space::Srgb | Space::ExtendedSrgb => "sRGB IEC61966-2.1",
            Space::DisplayP3 => "Display P3",
            Space::AdobeRgb => "Adobe RGB (1998)",
            Space::GenericRgb => "Generic RGB",
            Space::DeviceRgb => "Device RGB",
            Space::GenericGray => "Generic Gray",
            Space::Gamma22Gray | Space::ExtendedGamma22Gray => "Generic Gray Gamma 2.2 Profile",
            Space::DeviceGray => "Device Gray",
            Space::GenericCmyk => "Generic CMYK",
            Space::DeviceCmyk => "Device CMYK",
        }
    }

    /// The old name of the space a color in it reports.
    fn color_space_name(self) -> &'static str {
        match self {
            Space::GenericRgb => "NSCalibratedRGBColorSpace",
            Space::DeviceRgb => "NSDeviceRGBColorSpace",
            Space::GenericGray => "NSCalibratedWhiteColorSpace",
            Space::DeviceGray => "NSDeviceWhiteColorSpace",
            Space::DeviceCmyk => "NSDeviceCMYKColorSpace",
            _ => "NSCustomColorSpace",
        }
    }

    fn from_name(name: &str) -> Option<Space> {
        Some(match name {
            "NSCalibratedRGBColorSpace" => Space::GenericRgb,
            "NSDeviceRGBColorSpace" => Space::DeviceRgb,
            "NSCalibratedWhiteColorSpace" => Space::GenericGray,
            "NSDeviceWhiteColorSpace" => Space::DeviceGray,
            "NSDeviceCMYKColorSpace" => Space::DeviceCmyk,
            _ => return None,
        })
    }
}

pub(crate) struct SpaceIvars {
    space: Space,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; spaces are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "NSColorSpace"]
    #[ivars = SpaceIvars]
    pub(crate) struct NSColorSpaceImpl;

    impl NSColorSpaceImpl {
        #[unsafe(method_id(sRGBColorSpace))]
        fn srgb() -> Retained<NSColorSpace> {
            space(Space::Srgb)
        }

        #[unsafe(method_id(extendedSRGBColorSpace))]
        fn extended_srgb() -> Retained<NSColorSpace> {
            space(Space::ExtendedSrgb)
        }

        #[unsafe(method_id(displayP3ColorSpace))]
        fn display_p3() -> Retained<NSColorSpace> {
            space(Space::DisplayP3)
        }

        #[unsafe(method_id(adobeRGB1998ColorSpace))]
        fn adobe_rgb() -> Retained<NSColorSpace> {
            space(Space::AdobeRgb)
        }

        #[unsafe(method_id(genericRGBColorSpace))]
        fn generic_rgb() -> Retained<NSColorSpace> {
            space(Space::GenericRgb)
        }

        #[unsafe(method_id(deviceRGBColorSpace))]
        fn device_rgb() -> Retained<NSColorSpace> {
            space(Space::DeviceRgb)
        }

        #[unsafe(method_id(genericGrayColorSpace))]
        fn generic_gray() -> Retained<NSColorSpace> {
            space(Space::GenericGray)
        }

        #[unsafe(method_id(genericGamma22GrayColorSpace))]
        fn gamma22_gray() -> Retained<NSColorSpace> {
            space(Space::Gamma22Gray)
        }

        #[unsafe(method_id(extendedGenericGamma22GrayColorSpace))]
        fn extended_gamma22_gray() -> Retained<NSColorSpace> {
            space(Space::ExtendedGamma22Gray)
        }

        #[unsafe(method_id(deviceGrayColorSpace))]
        fn device_gray() -> Retained<NSColorSpace> {
            space(Space::DeviceGray)
        }

        #[unsafe(method_id(genericCMYKColorSpace))]
        fn generic_cmyk() -> Retained<NSColorSpace> {
            space(Space::GenericCmyk)
        }

        #[unsafe(method_id(deviceCMYKColorSpace))]
        fn device_cmyk() -> Retained<NSColorSpace> {
            space(Space::DeviceCmyk)
        }

        #[unsafe(method_id(availableColorSpacesWithModel:))]
        fn available(model: NSColorSpaceModel) -> Retained<NSArray<NSColorSpace>> {
            let spaces: Vec<Retained<NSColorSpace>> = SPACES
                .into_iter()
                .filter(|s| model == NSColorSpaceModel::Unknown || model_of(*s) == model)
                .map(space)
                .collect();
            NSArray::from_retained_slice(&spaces)
        }

        #[unsafe(method_id(initWithICCProfileData:))]
        fn init_with_icc(_this: Allocated<Self>, _data: &AnyObject) -> Option<Retained<Self>> {
            None
        }

        #[unsafe(method_id(ICCProfileData))]
        fn icc_profile_data(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(colorSpaceModel))]
        fn color_space_model(&self) -> NSColorSpaceModel {
            model_of(self.ivars().space)
        }

        #[unsafe(method(numberOfColorComponents))]
        fn number_of_color_components(&self) -> NSInteger {
            self.ivars().space.components() as NSInteger
        }

        #[unsafe(method_id(localizedName))]
        fn localized_name(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(self.ivars().space.localized_name()))
        }
    }

    unsafe impl NSObjectProtocol for NSColorSpaceImpl {}
);

fn model_of(s: Space) -> NSColorSpaceModel {
    match s.model() {
        Model::Gray => NSColorSpaceModel::Gray,
        Model::Rgb => NSColorSpaceModel::RGB,
        Model::Cmyk => NSColorSpaceModel::CMYK,
    }
}

/// The shared instance of `s`.
pub(crate) fn space(s: Space) -> Retained<NSColorSpace> {
    static ALL: OnceLock<[usize; 12]> = OnceLock::new();
    let all = ALL.get_or_init(|| {
        crate::load_shell::<NSColorSpace>();
        SPACES.map(|space| {
            let this = NSColorSpaceImpl::alloc().set_ivars(SpaceIvars { space });
            // SAFETY: NSObject's designated initializer.
            let this: Retained<NSColorSpaceImpl> = unsafe { msg_send![super(this), init] };
            Retained::into_raw(this) as usize
        })
    });
    let at = SPACES.iter().position(|x| *x == s).expect("a space");
    // SAFETY: the instances are never released; retaining one gives the
    // caller its own reference.
    unsafe { Retained::retain(all[at] as *mut NSColorSpace) }.expect("a color space")
}

pub(crate) fn space_of(s: &NSColorSpace) -> Space {
    // SAFETY: every NSColorSpace is an NSColorSpaceImpl.
    unsafe { &*(s as *const NSColorSpace).cast::<NSColorSpaceImpl>() }.ivars().space
}

// Conversions. RGB spaces other than Display P3 are sRGB here.

fn to_srgb(space: Space, c: &[f64]) -> [f64; 3] {
    match space.model() {
        Model::Gray => [c[0]; 3],
        Model::Cmyk => {
            let k = 1.0 - c[3];
            [(1.0 - c[0]) * k, (1.0 - c[1]) * k, (1.0 - c[2]) * k]
        }
        Model::Rgb if space == Space::DisplayP3 => p3_to_srgb([c[0], c[1], c[2]]),
        Model::Rgb => [c[0], c[1], c[2]],
    }
}

fn from_srgb(space: Space, rgb: [f64; 3]) -> Vec<f64> {
    match space.model() {
        Model::Gray => vec![gray_of(rgb)],
        Model::Cmyk => {
            let k = 1.0 - rgb[0].max(rgb[1]).max(rgb[2]);
            let f = |v: f64| if k >= 1.0 { 0.0 } else { (1.0 - v - k) / (1.0 - k) };
            vec![f(rgb[0]), f(rgb[1]), f(rgb[2]), k]
        }
        Model::Rgb if space == Space::DisplayP3 => srgb_to_p3(rgb).to_vec(),
        Model::Rgb => rgb.to_vec(),
    }
}

/// The gray of sRGB `rgb`: its luminance, weighed in linear light, encoded
/// again as sRGB encodes.
pub(crate) fn gray_of(rgb: [f64; 3]) -> f64 {
    encoded(0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2]))
}

/// sRGB's transfer curve, undone; odd, so extended values keep their sign.
fn linear(v: f64) -> f64 {
    let a = v.abs();
    let l = if a <= 0.04045 { a / 12.92 } else { ((a + 0.055) / 1.055).powf(2.4) };
    l.copysign(v)
}

fn encoded(v: f64) -> f64 {
    let a = v.abs();
    let e = if a <= 0.003_130_8 { a * 12.92 } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
    e.copysign(v)
}

fn mat(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// Display P3 (sRGB's transfer curve, P3 primaries, D65) to sRGB.
fn p3_to_srgb(c: [f64; 3]) -> [f64; 3] {
    const M: [[f64; 3]; 3] =
        [[1.224_940_2, -0.224_940_2, 0.0], [-0.042_056_9, 1.042_056_9, 0.0], [-0.019_637_6, -0.078_636_1, 1.098_273_7]];
    mat(M, c.map(linear)).map(encoded)
}

fn srgb_to_p3(c: [f64; 3]) -> [f64; 3] {
    const M: [[f64; 3]; 3] =
        [[0.822_461_9, 0.177_538_1, 0.0], [0.033_194_2, 0.966_805_8, 0.0], [0.017_082_6, 0.072_397_4, 0.910_519_9]];
    mat(M, c.map(linear)).map(encoded)
}

fn hsb_to_rgb(h: f64, s: f64, b: f64) -> [f64; 3] {
    let h = (h.rem_euclid(1.0)) * 6.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (b * (1.0 - s), b * (1.0 - s * f), b * (1.0 - s * (1.0 - f)));
    match i as u32 % 6 {
        0 => [b, t, p],
        1 => [q, b, p],
        2 => [p, b, t],
        3 => [p, q, b],
        4 => [t, p, b],
        _ => [b, p, q],
    }
}

fn rgb_to_hsb(c: [f64; 3]) -> [f64; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let d = max - min;
    let s = if max > 0.0 { d / max } else { 0.0 };
    let h = if d == 0.0 {
        0.0
    } else if max == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0) / 6.0
    } else if max == c[1] {
        ((c[2] - c[0]) / d + 2.0) / 6.0
    } else {
        ((c[0] - c[1]) / d + 4.0) / 6.0
    };
    [h, s, max]
}

type Provider = RcBlock<dyn Fn(NonNull<NSAppearance>) -> NonNull<NSColor>>;

/// What a color is.
#[derive(Clone)]
pub(crate) enum Repr {
    /// Components in `space`, alpha last.
    Components {
        space: Space,
        c: [f64; 5],
    },
    /// A system color, with another alpha when one was given it.
    Catalog {
        color: System,
        alpha: Option<f64>,
    },
    Dynamic {
        name: Option<Retained<NSString>>,
        provider: Provider,
        alpha: Option<f64>,
    },
    /// A system effect on another color, applied each time the color is
    /// used.
    Effect {
        base: Retained<NSColor>,
        effect: NSColorSystemEffect,
    },
    /// An image, and the thread it belongs to: `NSImage` isn't shared
    /// between threads, and colors are.
    Pattern(Retained<AnyObject>, std::thread::ThreadId),
}

pub(crate) struct ColorIvars {
    repr: Repr,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; colors are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "NSColor"]
    #[ivars = ColorIvars]
    pub(crate) struct NSColorImpl;

    // Colors from components.
    impl NSColorImpl {
        #[unsafe(method_id(colorWithSRGBRed:green:blue:alpha:))]
        fn srgb(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
            rgba(Space::Srgb, r, g, b, a)
        }

        #[unsafe(method_id(colorWithRed:green:blue:alpha:))]
        fn rgb(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
            rgba(srgb_for([r, g, b]), r, g, b, a)
        }

        #[unsafe(method_id(colorWithCalibratedRed:green:blue:alpha:))]
        fn calibrated(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
            rgba(Space::GenericRgb, r, g, b, a)
        }

        #[unsafe(method_id(colorWithDeviceRed:green:blue:alpha:))]
        fn device(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
            rgba(Space::DeviceRgb, r, g, b, a)
        }

        #[unsafe(method_id(colorWithDisplayP3Red:green:blue:alpha:))]
        fn display_p3(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
            rgba(Space::DisplayP3, r, g, b, a)
        }

        #[unsafe(method_id(colorWithWhite:alpha:))]
        fn white_alpha(w: f64, a: f64) -> Retained<NSColor> {
            gray(Space::Gamma22Gray, w, a)
        }

        #[unsafe(method_id(colorWithGenericGamma22White:alpha:))]
        fn gamma22_white(w: f64, a: f64) -> Retained<NSColor> {
            gray(Space::Gamma22Gray, w, a)
        }

        #[unsafe(method_id(colorWithCalibratedWhite:alpha:))]
        fn calibrated_white(w: f64, a: f64) -> Retained<NSColor> {
            gray(Space::GenericGray, w, a)
        }

        #[unsafe(method_id(colorWithDeviceWhite:alpha:))]
        fn device_white(w: f64, a: f64) -> Retained<NSColor> {
            gray(Space::DeviceGray, w, a)
        }

        #[unsafe(method_id(colorWithHue:saturation:brightness:alpha:))]
        fn hsb(h: f64, s: f64, b: f64, a: f64) -> Retained<NSColor> {
            let [r, g, bl] = hsb_to_rgb(h, s, b);
            rgba(srgb_for([r, g, bl]), r, g, bl, a)
        }

        #[unsafe(method_id(colorWithCalibratedHue:saturation:brightness:alpha:))]
        fn calibrated_hsb(h: f64, s: f64, b: f64, a: f64) -> Retained<NSColor> {
            let [r, g, bl] = hsb_to_rgb(h, s, b);
            rgba(Space::GenericRgb, r, g, bl, a)
        }

        #[unsafe(method_id(colorWithDeviceHue:saturation:brightness:alpha:))]
        fn device_hsb(h: f64, s: f64, b: f64, a: f64) -> Retained<NSColor> {
            let [r, g, bl] = hsb_to_rgb(h, s, b);
            rgba(Space::DeviceRgb, r, g, bl, a)
        }

        #[unsafe(method_id(colorWithColorSpace:hue:saturation:brightness:alpha:))]
        fn space_hsb(space: &NSColorSpace, h: f64, s: f64, b: f64, a: f64) -> Retained<NSColor> {
            let space = space_of(space);
            let [r, g, bl] = hsb_to_rgb(h, s, b);
            let rgb = if space.model() == Model::Rgb { space } else { Space::Srgb };
            rgba(rgb, r, g, bl, a)
        }

        #[unsafe(method_id(colorWithDeviceCyan:magenta:yellow:black:alpha:))]
        fn cmyk(c: f64, m: f64, y: f64, k: f64, a: f64) -> Retained<NSColor> {
            make(Repr::Components { space: Space::DeviceCmyk, c: [c, m, y, k, a] })
        }

        #[unsafe(method_id(colorWithColorSpace:components:count:))]
        fn with_components(space: &NSColorSpace, components: NonNull<f64>, count: NSInteger) -> Retained<NSColor> {
            let space = space_of(space);
            let n = space.components();
            let count = usize::try_from(count).unwrap_or(0).min(n + 1);
            // SAFETY: the caller passes `count` components.
            let given = unsafe { std::slice::from_raw_parts(components.as_ptr(), count) };
            let mut c = [0.0; 5];
            c[..count].copy_from_slice(given);
            if count <= n {
                c[n] = 1.0;
            } else if n < 4 {
                // Alpha goes last, after the space's components.
                c[n] = given[n];
            }
            make(Repr::Components { space, c })
        }

        #[unsafe(method_id(colorWithPatternImage:))]
        fn pattern(image: &AnyObject) -> Retained<NSColor> {
            make(Repr::Pattern(image.retain(), std::thread::current().id()))
        }

        #[unsafe(method_id(colorNamed:))]
        fn named(_name: &NSColorName) -> Option<Retained<NSColor>> {
            None
        }

        #[unsafe(method_id(colorNamed:bundle:))]
        fn named_bundle(_name: &NSColorName, _bundle: Option<&AnyObject>) -> Option<Retained<NSColor>> {
            None
        }

        #[unsafe(method_id(colorWithCatalogName:colorName:))]
        fn with_catalog(_catalog: &NSColorListName, name: &NSColorName) -> Option<Retained<NSColor>> {
            let name = name.to_string();
            System::ALL.iter().find(|c| c.name() == name).map(|&c| catalog(c))
        }

        #[unsafe(method_id(colorWithName:dynamicProvider:))]
        fn dynamic(
            name: Option<&NSColorName>,
            provider: &DynBlock<dyn Fn(NonNull<NSAppearance>) -> NonNull<NSColor>>,
        ) -> Retained<NSColor> {
            make(Repr::Dynamic { name: name.map(|n| NSString::from_str(&n.to_string())), provider: provider.copy(), alpha: None })
        }
    }

    // The fixed colors.
    impl NSColorImpl {
        #[unsafe(method_id(blackColor))]
        fn black() -> Retained<NSColor> {
            gray(Space::Gamma22Gray, 0.0, 1.0)
        }

        #[unsafe(method_id(darkGrayColor))]
        fn dark_gray() -> Retained<NSColor> {
            gray(Space::Gamma22Gray, 1.0 / 3.0, 1.0)
        }

        #[unsafe(method_id(lightGrayColor))]
        fn light_gray() -> Retained<NSColor> {
            gray(Space::Gamma22Gray, 2.0 / 3.0, 1.0)
        }

        #[unsafe(method_id(whiteColor))]
        fn white() -> Retained<NSColor> {
            gray(Space::Gamma22Gray, 1.0, 1.0)
        }

        #[unsafe(method_id(grayColor))]
        fn gray_color() -> Retained<NSColor> {
            gray(Space::Gamma22Gray, 0.5, 1.0)
        }

        #[unsafe(method_id(clearColor))]
        fn clear() -> Retained<NSColor> {
            gray(Space::Gamma22Gray, 0.0, 0.0)
        }

        #[unsafe(method_id(redColor))]
        fn red() -> Retained<NSColor> {
            rgba(Space::Srgb, 1.0, 0.0, 0.0, 1.0)
        }

        #[unsafe(method_id(greenColor))]
        fn green() -> Retained<NSColor> {
            rgba(Space::Srgb, 0.0, 1.0, 0.0, 1.0)
        }

        #[unsafe(method_id(blueColor))]
        fn blue() -> Retained<NSColor> {
            rgba(Space::Srgb, 0.0, 0.0, 1.0, 1.0)
        }

        #[unsafe(method_id(cyanColor))]
        fn cyan() -> Retained<NSColor> {
            rgba(Space::Srgb, 0.0, 1.0, 1.0, 1.0)
        }

        #[unsafe(method_id(yellowColor))]
        fn yellow() -> Retained<NSColor> {
            rgba(Space::Srgb, 1.0, 1.0, 0.0, 1.0)
        }

        #[unsafe(method_id(magentaColor))]
        fn magenta() -> Retained<NSColor> {
            rgba(Space::Srgb, 1.0, 0.0, 1.0, 1.0)
        }

        #[unsafe(method_id(orangeColor))]
        fn orange() -> Retained<NSColor> {
            rgba(Space::Srgb, 1.0, 0.5, 0.0, 1.0)
        }

        #[unsafe(method_id(purpleColor))]
        fn purple() -> Retained<NSColor> {
            rgba(Space::Srgb, 0.5, 0.0, 0.5, 1.0)
        }

        #[unsafe(method_id(brownColor))]
        fn brown() -> Retained<NSColor> {
            rgba(Space::Srgb, 0.6, 0.4, 0.2, 1.0)
        }
    }

    // System colors: one shared instance each.
    impl NSColorImpl {
        #[unsafe(method_id(labelColor))]
        fn label() -> Retained<NSColor> {
            catalog(System::Label)
        }

        #[unsafe(method_id(secondaryLabelColor))]
        fn secondary_label() -> Retained<NSColor> {
            catalog(System::SecondaryLabel)
        }

        #[unsafe(method_id(tertiaryLabelColor))]
        fn tertiary_label() -> Retained<NSColor> {
            catalog(System::TertiaryLabel)
        }

        #[unsafe(method_id(quaternaryLabelColor))]
        fn quaternary_label() -> Retained<NSColor> {
            catalog(System::QuaternaryLabel)
        }

        #[unsafe(method_id(quinaryLabelColor))]
        fn quinary_label() -> Retained<NSColor> {
            catalog(System::QuinaryLabel)
        }

        #[unsafe(method_id(linkColor))]
        fn link() -> Retained<NSColor> {
            catalog(System::Link)
        }

        #[unsafe(method_id(placeholderTextColor))]
        fn placeholder_text() -> Retained<NSColor> {
            catalog(System::PlaceholderText)
        }

        #[unsafe(method_id(windowFrameTextColor))]
        fn window_frame_text() -> Retained<NSColor> {
            catalog(System::WindowFrameText)
        }

        #[unsafe(method_id(selectedMenuItemTextColor))]
        fn selected_menu_item_text() -> Retained<NSColor> {
            catalog(System::SelectedMenuItemText)
        }

        #[unsafe(method_id(alternateSelectedControlTextColor))]
        fn alternate_selected_control_text() -> Retained<NSColor> {
            catalog(System::AlternateSelectedControlText)
        }

        #[unsafe(method_id(headerTextColor))]
        fn header_text() -> Retained<NSColor> {
            catalog(System::HeaderText)
        }

        #[unsafe(method_id(separatorColor))]
        fn separator() -> Retained<NSColor> {
            catalog(System::Separator)
        }

        #[unsafe(method_id(gridColor))]
        fn grid() -> Retained<NSColor> {
            catalog(System::Grid)
        }

        #[unsafe(method_id(windowBackgroundColor))]
        fn window_background() -> Retained<NSColor> {
            catalog(System::WindowBackground)
        }

        #[unsafe(method_id(underPageBackgroundColor))]
        fn under_page_background() -> Retained<NSColor> {
            catalog(System::UnderPageBackground)
        }

        #[unsafe(method_id(controlBackgroundColor))]
        fn control_background() -> Retained<NSColor> {
            catalog(System::ControlBackground)
        }

        #[unsafe(method_id(selectedContentBackgroundColor))]
        fn selected_content_background() -> Retained<NSColor> {
            catalog(System::SelectedContentBackground)
        }

        #[unsafe(method_id(unemphasizedSelectedContentBackgroundColor))]
        fn unemphasized_selected_content_background() -> Retained<NSColor> {
            catalog(System::UnemphasizedSelectedContentBackground)
        }

        #[unsafe(method_id(alternatingContentBackgroundColors))]
        fn alternating_content_background() -> Retained<NSArray<NSColor>> {
            NSArray::from_retained_slice(&[catalog(System::AlternatingContentBackground), catalog(System::AlternatingRowBackground)])
        }

        #[unsafe(method_id(controlAlternatingRowBackgroundColors))]
        fn control_alternating_row_background() -> Retained<NSArray<NSColor>> {
            NSArray::from_retained_slice(&[catalog(System::AlternatingContentBackground), catalog(System::AlternatingRowBackground)])
        }

        #[unsafe(method_id(findHighlightColor))]
        fn find_highlight() -> Retained<NSColor> {
            catalog(System::FindHighlight)
        }

        #[unsafe(method_id(textColor))]
        fn text() -> Retained<NSColor> {
            catalog(System::Text)
        }

        #[unsafe(method_id(textBackgroundColor))]
        fn text_background() -> Retained<NSColor> {
            catalog(System::TextBackground)
        }

        #[unsafe(method_id(textInsertionPointColor))]
        fn text_insertion_point() -> Retained<NSColor> {
            catalog(System::TextInsertionPoint)
        }

        #[unsafe(method_id(selectedTextColor))]
        fn selected_text() -> Retained<NSColor> {
            catalog(System::SelectedText)
        }

        #[unsafe(method_id(selectedTextBackgroundColor))]
        fn selected_text_background() -> Retained<NSColor> {
            catalog(System::SelectedTextBackground)
        }

        #[unsafe(method_id(unemphasizedSelectedTextBackgroundColor))]
        fn unemphasized_selected_text_background() -> Retained<NSColor> {
            catalog(System::UnemphasizedSelectedTextBackground)
        }

        #[unsafe(method_id(unemphasizedSelectedTextColor))]
        fn unemphasized_selected_text() -> Retained<NSColor> {
            catalog(System::UnemphasizedSelectedText)
        }

        #[unsafe(method_id(controlColor))]
        fn control() -> Retained<NSColor> {
            catalog(System::Control)
        }

        #[unsafe(method_id(controlTextColor))]
        fn control_text() -> Retained<NSColor> {
            catalog(System::ControlText)
        }

        #[unsafe(method_id(selectedControlColor))]
        fn selected_control() -> Retained<NSColor> {
            catalog(System::SelectedControl)
        }

        #[unsafe(method_id(selectedControlTextColor))]
        fn selected_control_text() -> Retained<NSColor> {
            catalog(System::SelectedControlText)
        }

        #[unsafe(method_id(disabledControlTextColor))]
        fn disabled_control_text() -> Retained<NSColor> {
            catalog(System::DisabledControlText)
        }

        #[unsafe(method_id(keyboardFocusIndicatorColor))]
        fn keyboard_focus_indicator() -> Retained<NSColor> {
            catalog(System::KeyboardFocusIndicator)
        }

        #[unsafe(method_id(scrubberTexturedBackgroundColor))]
        fn scrubber_textured_background() -> Retained<NSColor> {
            catalog(System::ScrubberTexturedBackground)
        }

        #[unsafe(method_id(controlAccentColor))]
        fn control_accent() -> Retained<NSColor> {
            catalog(System::ControlAccent)
        }

        #[unsafe(method_id(highlightColor))]
        fn highlight() -> Retained<NSColor> {
            catalog(System::Highlight)
        }

        #[unsafe(method_id(shadowColor))]
        fn shadow() -> Retained<NSColor> {
            catalog(System::Shadow)
        }

        #[unsafe(method_id(systemRedColor))]
        fn system_red() -> Retained<NSColor> {
            catalog(System::Red)
        }

        #[unsafe(method_id(systemGreenColor))]
        fn system_green() -> Retained<NSColor> {
            catalog(System::Green)
        }

        #[unsafe(method_id(systemBlueColor))]
        fn system_blue() -> Retained<NSColor> {
            catalog(System::Blue)
        }

        #[unsafe(method_id(systemOrangeColor))]
        fn system_orange() -> Retained<NSColor> {
            catalog(System::Orange)
        }

        #[unsafe(method_id(systemYellowColor))]
        fn system_yellow() -> Retained<NSColor> {
            catalog(System::Yellow)
        }

        #[unsafe(method_id(systemBrownColor))]
        fn system_brown() -> Retained<NSColor> {
            catalog(System::Brown)
        }

        #[unsafe(method_id(systemPinkColor))]
        fn system_pink() -> Retained<NSColor> {
            catalog(System::Pink)
        }

        #[unsafe(method_id(systemPurpleColor))]
        fn system_purple() -> Retained<NSColor> {
            catalog(System::Purple)
        }

        #[unsafe(method_id(systemGrayColor))]
        fn system_gray() -> Retained<NSColor> {
            catalog(System::Gray)
        }

        #[unsafe(method_id(systemTealColor))]
        fn system_teal() -> Retained<NSColor> {
            catalog(System::Teal)
        }

        #[unsafe(method_id(systemIndigoColor))]
        fn system_indigo() -> Retained<NSColor> {
            catalog(System::Indigo)
        }

        #[unsafe(method_id(systemMintColor))]
        fn system_mint() -> Retained<NSColor> {
            catalog(System::Mint)
        }

        #[unsafe(method_id(systemCyanColor))]
        fn system_cyan() -> Retained<NSColor> {
            catalog(System::Cyan)
        }

        #[unsafe(method_id(systemFillColor))]
        fn system_fill() -> Retained<NSColor> {
            catalog(System::Fill)
        }

        #[unsafe(method_id(secondarySystemFillColor))]
        fn secondary_system_fill() -> Retained<NSColor> {
            catalog(System::SecondaryFill)
        }

        #[unsafe(method_id(tertiarySystemFillColor))]
        fn tertiary_system_fill() -> Retained<NSColor> {
            catalog(System::TertiaryFill)
        }

        #[unsafe(method_id(quaternarySystemFillColor))]
        fn quaternary_system_fill() -> Retained<NSColor> {
            catalog(System::QuaternaryFill)
        }

        #[unsafe(method_id(quinarySystemFillColor))]
        fn quinary_system_fill() -> Retained<NSColor> {
            catalog(System::QuinaryFill)
        }

        #[unsafe(method_id(controlHighlightColor))]
        fn control_highlight() -> Retained<NSColor> {
            catalog(System::ControlHighlight)
        }

        #[unsafe(method_id(controlLightHighlightColor))]
        fn control_light_highlight() -> Retained<NSColor> {
            catalog(System::ControlLightHighlight)
        }

        #[unsafe(method_id(controlShadowColor))]
        fn control_shadow() -> Retained<NSColor> {
            catalog(System::ControlShadow)
        }

        #[unsafe(method_id(controlDarkShadowColor))]
        fn control_dark_shadow() -> Retained<NSColor> {
            catalog(System::ControlDarkShadow)
        }

        #[unsafe(method_id(scrollBarColor))]
        fn scroll_bar() -> Retained<NSColor> {
            catalog(System::ScrollBar)
        }

        #[unsafe(method_id(knobColor))]
        fn knob() -> Retained<NSColor> {
            catalog(System::Knob)
        }

        #[unsafe(method_id(selectedKnobColor))]
        fn selected_knob() -> Retained<NSColor> {
            catalog(System::SelectedKnob)
        }

        #[unsafe(method_id(windowFrameColor))]
        fn window_frame() -> Retained<NSColor> {
            catalog(System::WindowFrame)
        }

        #[unsafe(method_id(selectedMenuItemColor))]
        fn selected_menu_item() -> Retained<NSColor> {
            catalog(System::SelectedMenuItem)
        }

        #[unsafe(method_id(headerColor))]
        fn header() -> Retained<NSColor> {
            catalog(System::Header)
        }

        #[unsafe(method_id(secondarySelectedControlColor))]
        fn secondary_selected_control() -> Retained<NSColor> {
            catalog(System::SecondarySelectedControl)
        }

        #[unsafe(method_id(alternateSelectedControlColor))]
        fn alternate_selected_control() -> Retained<NSColor> {
            catalog(System::AlternateSelectedControl)
        }

        #[unsafe(method(currentControlTint))]
        fn current_control_tint() -> NSControlTint {
            NSControlTint::DefaultControlTint
        }

        #[unsafe(method_id(colorForControlTint:))]
        fn color_for_control_tint(tint: NSControlTint) -> Retained<NSColor> {
            if tint == NSControlTint::GraphiteControlTint {
                catalog(System::Gray)
            } else {
                catalog(System::ControlAccent)
            }
        }
    }

    // Using a color.
    impl NSColorImpl {
        #[unsafe(method(set))]
        fn set(&self) {
            let c = resolve_impl(self);
            crate::context::with_state(|st| {
                st.gs.fill = c;
                st.gs.stroke = c;
            });
        }

        #[unsafe(method(setFill))]
        fn set_fill(&self) {
            let c = resolve_impl(self);
            crate::context::with_state(|st| st.gs.fill = c);
        }

        #[unsafe(method(setStroke))]
        fn set_stroke(&self) {
            let c = resolve_impl(self);
            crate::context::with_state(|st| st.gs.stroke = c);
        }

        #[unsafe(method(drawSwatchInRect:))]
        fn draw_swatch(&self, rect: NSRect) {
            let c = resolve_impl(self);
            crate::context::with_state(|st| st.fill_rect(rect, c, Blend::SourceOver));
        }
    }

    // Derived colors.
    impl NSColorImpl {
        #[unsafe(method_id(colorWithAlphaComponent:))]
        fn with_alpha(&self, alpha: f64) -> Retained<NSColor> {
            let alpha = alpha.clamp(0.0, 1.0);
            make(match &self.ivars().repr {
                Repr::Components { space, c } => {
                    let mut c = *c;
                    c[space.components()] = alpha;
                    Repr::Components { space: *space, c }
                }
                // A system color is resolved now, in the appearance of the
                // moment, as AppKit does; a dynamic one stays dynamic.
                Repr::Catalog { .. } => {
                    let c = resolve_impl(self).map(f64::from);
                    Repr::Components { space: Space::Srgb, c: [c[0], c[1], c[2], alpha, 0.0] }
                }
                Repr::Dynamic { name, provider, .. } => {
                    Repr::Dynamic { name: name.clone(), provider: provider.clone(), alpha: Some(alpha) }
                }
                Repr::Effect { .. } => {
                    let c = resolve_impl(self).map(f64::from);
                    Repr::Components { space: Space::Srgb, c: [c[0], c[1], c[2], alpha, 0.0] }
                }
                Repr::Pattern(p, thread) => Repr::Pattern(p.clone(), *thread),
            })
        }

        #[unsafe(method_id(blendedColorWithFraction:ofColor:))]
        fn blended(&self, fraction: f64, other: &NSColor) -> Option<Retained<NSColor>> {
            Some(blend_colors(self, fraction, other))
        }

        #[unsafe(method_id(highlightWithLevel:))]
        fn highlight_with_level(&self, level: f64) -> Option<Retained<NSColor>> {
            Some(blend_colors(self, level, &white_color()))
        }

        #[unsafe(method_id(shadowWithLevel:))]
        fn shadow_with_level(&self, level: f64) -> Option<Retained<NSColor>> {
            Some(blend_colors(self, level, &black_color()))
        }

        // The effect depends on the appearance, so it's worked out each
        // time the color is used, whatever the color (a catalog color, as
        // AppKit makes it).
        #[unsafe(method_id(colorWithSystemEffect:))]
        fn with_system_effect(&self, effect: NSColorSystemEffect) -> Retained<NSColor> {
            match &self.ivars().repr {
                Repr::Pattern(..) => self.as_color().retain(),
                _ => make(Repr::Effect { base: self.as_color().retain(), effect }),
            }
        }
    }

    // Types and spaces.
    impl NSColorImpl {
        #[unsafe(method(type))]
        fn kind(&self) -> NSColorType {
            match &self.ivars().repr {
                Repr::Components { .. } => NSColorType::ComponentBased,
                // Given an alpha, AppKit's catalog colors say they're made
                // of components (and still follow the appearance).
                Repr::Catalog { alpha: Some(_), .. } => NSColorType::ComponentBased,
                Repr::Catalog { .. } | Repr::Dynamic { .. } | Repr::Effect { .. } => NSColorType::Catalog,
                Repr::Pattern(..) => NSColorType::Pattern,
            }
        }

        #[unsafe(method_id(colorUsingType:))]
        fn using_type(&self, kind: NSColorType) -> Option<Retained<NSColor>> {
            match (&self.ivars().repr, kind) {
                (Repr::Components { .. }, NSColorType::ComponentBased) => Some(self.as_color().retain()),
                (Repr::Catalog { .. } | Repr::Dynamic { .. } | Repr::Effect { .. }, NSColorType::Catalog) => {
                    Some(self.as_color().retain())
                }
                (Repr::Pattern(..), NSColorType::Pattern) => Some(self.as_color().retain()),
                (Repr::Pattern(..), _) => None,
                (_, NSColorType::ComponentBased) => {
                    let c = resolve_impl(self).map(f64::from);
                    Some(rgba(Space::Srgb, c[0], c[1], c[2], c[3]))
                }
                _ => None,
            }
        }

        #[unsafe(method_id(colorUsingColorSpace:))]
        fn using_color_space(&self, target: &NSColorSpace) -> Option<Retained<NSColor>> {
            convert(self, space_of(target))
        }

        #[unsafe(method_id(colorUsingColorSpaceName:))]
        fn using_color_space_name(&self, name: Option<&NSColorSpaceName>) -> Option<Retained<NSColor>> {
            using_space_name(self, name)
        }

        #[unsafe(method_id(colorUsingColorSpaceName:device:))]
        fn using_color_space_name_device(
            &self,
            name: Option<&NSColorSpaceName>,
            _device: Option<&AnyObject>,
        ) -> Option<Retained<NSColor>> {
            using_space_name(self, name)
        }

        #[unsafe(method_id(colorSpace))]
        fn color_space(&self) -> Retained<NSColorSpace> {
            match &self.ivars().repr {
                Repr::Components { space: s, .. } => space(*s),
                _ => space(Space::Srgb),
            }
        }

        #[unsafe(method_id(colorSpaceName))]
        fn color_space_name(&self) -> Retained<NSColorSpaceName> {
            NSString::from_str(match &self.ivars().repr {
                Repr::Components { space, .. } => space.color_space_name(),
                Repr::Catalog { alpha: Some(_), .. } => "NSCustomColorSpace",
                Repr::Catalog { .. } | Repr::Dynamic { .. } | Repr::Effect { .. } => "NSNamedColorSpace",
                Repr::Pattern(..) => "NSPatternColorSpace",
            })
        }

        #[unsafe(method(numberOfComponents))]
        fn number_of_components(&self) -> NSInteger {
            match &self.ivars().repr {
                Repr::Components { space, .. } => space.components() as NSInteger + 1,
                _ => 4,
            }
        }

        #[unsafe(method(getComponents:))]
        fn get_components(&self, out: NonNull<f64>) {
            let (n, c) = self.components();
            // SAFETY: the caller passes room for numberOfComponents values.
            unsafe { std::ptr::copy_nonoverlapping(c.as_ptr(), out.as_ptr(), n) };
        }

        #[unsafe(method_id(catalogNameComponent))]
        fn catalog_name_component(&self) -> Retained<NSString> {
            catalog_name(self)
        }

        #[unsafe(method_id(colorNameComponent))]
        fn color_name_component(&self) -> Retained<NSString> {
            color_name(self)
        }

        #[unsafe(method_id(localizedCatalogNameComponent))]
        fn localized_catalog_name_component(&self) -> Retained<NSString> {
            catalog_name(self)
        }

        #[unsafe(method_id(localizedColorNameComponent))]
        fn localized_color_name_component(&self) -> Retained<NSString> {
            color_name(self)
        }

        #[unsafe(method_id(patternImage))]
        fn pattern_image(&self) -> Option<Retained<AnyObject>> {
            match &self.ivars().repr {
                // Only on the image's own thread: an image used from two
                // threads at once would race on its state (Sidestep's images
                // aren't shared between threads).
                Repr::Pattern(image, thread) if *thread == std::thread::current().id() => Some(image.clone()),
                _ => None,
            }
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSColor> {
            // Immutable: a copy is the color itself.
            self.as_color().retain()
        }
    }

    // Components.
    impl NSColorImpl {
        #[unsafe(method(redComponent))]
        fn red_component(&self) -> f64 {
            self.srgb3()[0]
        }

        #[unsafe(method(greenComponent))]
        fn green_component(&self) -> f64 {
            self.srgb3()[1]
        }

        #[unsafe(method(blueComponent))]
        fn blue_component(&self) -> f64 {
            self.srgb3()[2]
        }

        #[unsafe(method(alphaComponent))]
        fn alpha_component(&self) -> f64 {
            self.alpha()
        }

        #[unsafe(method(whiteComponent))]
        fn white_component(&self) -> f64 {
            self.white_level()
        }

        #[unsafe(method(hueComponent))]
        fn hue_component(&self) -> f64 {
            rgb_to_hsb(self.srgb3())[0]
        }

        #[unsafe(method(saturationComponent))]
        fn saturation_component(&self) -> f64 {
            rgb_to_hsb(self.srgb3())[1]
        }

        #[unsafe(method(brightnessComponent))]
        fn brightness_component(&self) -> f64 {
            rgb_to_hsb(self.srgb3())[2]
        }

        #[unsafe(method(cyanComponent))]
        fn cyan_component(&self) -> f64 {
            self.cmyk4()[0]
        }

        #[unsafe(method(magentaComponent))]
        fn magenta_component(&self) -> f64 {
            self.cmyk4()[1]
        }

        #[unsafe(method(yellowComponent))]
        fn yellow_component(&self) -> f64 {
            self.cmyk4()[2]
        }

        #[unsafe(method(blackComponent))]
        fn black_component(&self) -> f64 {
            self.cmyk4()[3]
        }

        #[unsafe(method(getRed:green:blue:alpha:))]
        fn get_rgba(&self, r: *mut f64, g: *mut f64, b: *mut f64, a: *mut f64) {
            let [rr, gg, bb] = self.srgb3();
            // SAFETY: each pointer is null or writable, as the caller
            // promises.
            unsafe { store(&[(r, rr), (g, gg), (b, bb), (a, self.alpha())]) };
        }

        #[unsafe(method(getHue:saturation:brightness:alpha:))]
        fn get_hsba(&self, h: *mut f64, s: *mut f64, b: *mut f64, a: *mut f64) {
            let [hh, ss, bb] = rgb_to_hsb(self.srgb3());
            // SAFETY: as above.
            unsafe { store(&[(h, hh), (s, ss), (b, bb), (a, self.alpha())]) };
        }

        #[unsafe(method(getWhite:alpha:))]
        fn get_white(&self, w: *mut f64, a: *mut f64) {
            // SAFETY: as above.
            unsafe { store(&[(w, self.white_level()), (a, self.alpha())]) };
        }

        #[unsafe(method(getCyan:magenta:yellow:black:alpha:))]
        fn get_cmyka(&self, c: *mut f64, m: *mut f64, y: *mut f64, k: *mut f64, a: *mut f64) {
            let [cc, mm, yy, kk] = self.cmyk4();
            // SAFETY: as above.
            unsafe { store(&[(c, cc), (m, mm), (y, yy), (k, kk), (a, self.alpha())]) };
        }
    }

    unsafe impl NSObjectProtocol for NSColorImpl {
        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|o| equal(self, o))
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            match &self.ivars().repr {
                Repr::Components { space, c } => {
                    let mut h = *space as usize;
                    for v in c {
                        h = h.wrapping_mul(31).wrapping_add((v * 65535.0).round() as i64 as usize);
                    }
                    h
                }
                Repr::Catalog { color, .. } => 0x5eed ^ (*color as usize),
                _ => self as *const Self as usize >> 4,
            }
        }
    }

    unsafe impl NSCopying for NSColorImpl {}
);

fn equal(this: &NSColorImpl, other: &AnyObject) -> bool {
    if std::ptr::eq(other as *const AnyObject, (this as *const NSColorImpl).cast()) {
        return true;
    }
    let Some(other) = other.downcast_ref::<NSColor>() else { return false };
    let other = imp(other);
    match (&this.ivars().repr, &other.ivars().repr) {
        (Repr::Components { space: s1, c: c1 }, Repr::Components { space: s2, c: c2 }) => s1 == s2 && c1 == c2,
        (Repr::Catalog { color: a, alpha: x }, Repr::Catalog { color: b, alpha: y }) => a == b && x == y,
        (Repr::Effect { base: a, effect: x }, Repr::Effect { base: b, effect: y }) => x == y && equal(imp(a), b),
        (Repr::Pattern(a, _), Repr::Pattern(b, _)) => std::ptr::eq(&**a, &**b),
        _ => false,
    }
}

fn blend_colors(this: &NSColorImpl, fraction: f64, other: &NSColor) -> Retained<NSColor> {
    let (a, b) = (resolve_impl(this), resolve(other));
    let t = fraction.clamp(0.0, 1.0);
    let mix = |i: usize| f64::from(a[i]) * (1.0 - t) + f64::from(b[i]) * t;
    rgba(Space::GenericRgb, mix(0), mix(1), mix(2), mix(3))
}

fn using_space_name(this: &NSColorImpl, name: Option<&NSColorSpaceName>) -> Option<Retained<NSColor>> {
    let name = name.map(|n| n.to_string()).unwrap_or_else(|| "NSCalibratedRGBColorSpace".into());
    match (&this.ivars().repr, Space::from_name(&name)) {
        (_, Some(space)) => convert(this, space),
        (Repr::Catalog { .. } | Repr::Dynamic { .. } | Repr::Effect { .. }, None) if name == "NSNamedColorSpace" => {
            Some(this.as_color().retain())
        }
        (Repr::Pattern(..), None) if name == "NSPatternColorSpace" => Some(this.as_color().retain()),
        _ => None,
    }
}

fn catalog_name(this: &NSColorImpl) -> Retained<NSString> {
    NSString::from_str(match &this.ivars().repr {
        Repr::Catalog { .. } | Repr::Dynamic { .. } | Repr::Effect { .. } => "System",
        _ => "",
    })
}

fn color_name(this: &NSColorImpl) -> Retained<NSString> {
    match &this.ivars().repr {
        Repr::Catalog { color, .. } => NSString::from_str(color.name()),
        Repr::Dynamic { name: Some(name), .. } => name.clone(),
        Repr::Effect { base, .. } => color_name(imp(base)),
        _ => NSString::new(),
    }
}

/// # Safety
///
/// Each pointer must be null or valid to write.
unsafe fn store(values: &[(*mut f64, f64)]) {
    for &(p, v) in values {
        if !p.is_null() {
            // SAFETY: as the caller promises.
            unsafe { *p = v };
        }
    }
}

/// Clamp components to 0…1 unless `space` is an extended one.
fn clamp_to(space: Space, mut c: [f64; 5]) -> [f64; 5] {
    if !matches!(space, Space::ExtendedSrgb | Space::ExtendedGamma22Gray) {
        for v in &mut c[..=space.components()] {
            *v = v.clamp(0.0, 1.0);
        }
    }
    c
}

/// sRGB, or extended sRGB for components beyond it (`colorWithRed:…`).
fn srgb_for(rgb: [f64; 3]) -> Space {
    if rgb.iter().all(|v| (0.0..=1.0).contains(v)) { Space::Srgb } else { Space::ExtendedSrgb }
}

/// `c` with a system effect in a light or dark appearance, as macOS works
/// it out (`color_appearance.rs`, `system_effects`). Disabled fades: to
/// 35% of its alpha in a light appearance, half in a dark one. Pressed,
/// deep-pressed and rollover (deep-pressed again in a light appearance)
/// add a step to the color taken premultiplied, the results reported as
/// its components, in whole 255ths: in a dark appearance they add to the
/// color and the alpha; in a light one they darken the color by a
/// fraction and add twice that to the alpha, and what the alpha would
/// gain past 1 comes off the color.
pub(crate) fn with_effect(c: Color, effect: NSColorSystemEffect, dark: bool) -> Color {
    let step = match (effect, dark) {
        (NSColorSystemEffect::Pressed, false) => 20.0,
        (NSColorSystemEffect::DeepPressed | NSColorSystemEffect::Rollover, false) => 36.0,
        (NSColorSystemEffect::Pressed, true) => 46.0,
        (NSColorSystemEffect::DeepPressed, true) => 82.0,
        (NSColorSystemEffect::Rollover, true) => 61.0,
        (NSColorSystemEffect::Disabled, _) => {
            return [c[0], c[1], c[2], c[3] * if dark { 0.5 } else { 0.35 }];
        }
        _ => return c,
    } / 255.0;
    let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() / 255.0;
    let [r, g, b, a] = c.map(|v| q(f64::from(v)));
    let out = if dark {
        [r * a + step, g * a + step, b * a + step, a + step]
    } else {
        let alpha = a * (1.0 - step) + 2.0 * step;
        let over = (alpha - 1.0).max(0.0);
        let darker = |v: f64| v * a * (1.0 - step) - over;
        [darker(r), darker(g), darker(b), alpha]
    };
    out.map(|v| q(v) as f32)
}

impl NSColorImpl {
    fn as_color(&self) -> &NSColor {
        // SAFETY: NSColorImpl is the class NSColor names.
        unsafe { &*(self as *const Self).cast::<NSColor>() }
    }

    /// Components (alpha last) and how many.
    fn components(&self) -> (usize, [f64; 5]) {
        match &self.ivars().repr {
            Repr::Components { space, c } => (space.components() + 1, *c),
            _ => {
                let c = resolve_impl(self).map(f64::from);
                (4, [c[0], c[1], c[2], c[3], 0.0])
            }
        }
    }

    /// As sRGB red, green and blue.
    fn srgb3(&self) -> [f64; 3] {
        match &self.ivars().repr {
            Repr::Components { space, c } => to_srgb(*space, c),
            _ => {
                let c = resolve_impl(self);
                [c[0], c[1], c[2]].map(f64::from)
            }
        }
    }

    fn alpha(&self) -> f64 {
        match &self.ivars().repr {
            Repr::Components { space, c } => c[space.components()],
            _ => f64::from(resolve_impl(self)[3]),
        }
    }

    fn white_level(&self) -> f64 {
        match &self.ivars().repr {
            Repr::Components { space, c } if space.model() == Model::Gray => c[0],
            _ => from_srgb(Space::Gamma22Gray, self.srgb3())[0],
        }
    }

    fn cmyk4(&self) -> [f64; 4] {
        match &self.ivars().repr {
            Repr::Components { space, c } if space.model() == Model::Cmyk => [c[0], c[1], c[2], c[3]],
            _ => {
                let v = from_srgb(Space::DeviceCmyk, self.srgb3());
                [v[0], v[1], v[2], v[3]]
            }
        }
    }
}

fn make(repr: Repr) -> Retained<NSColor> {
    crate::load_shell::<NSColor>();
    let this = NSColorImpl::alloc().set_ivars(ColorIvars { repr });
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSColorImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSColorImpl is the class NSColor names.
    unsafe { Retained::cast_unchecked(this) }
}

fn rgba(space: Space, r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
    make(Repr::Components { space, c: [r, g, b, a, 0.0] })
}

fn gray(space: Space, w: f64, a: f64) -> Retained<NSColor> {
    make(Repr::Components { space, c: [w, a, 0.0, 0.0, 0.0] })
}

fn white_color() -> Retained<NSColor> {
    gray(Space::Gamma22Gray, 1.0, 1.0)
}

fn black_color() -> Retained<NSColor> {
    gray(Space::Gamma22Gray, 0.0, 1.0)
}

/// The shared instance of a system color.
pub(crate) fn catalog(c: System) -> Retained<NSColor> {
    static ALL: OnceLock<Vec<usize>> = OnceLock::new();
    let all = ALL.get_or_init(|| {
        System::ALL
            .iter()
            .map(|&color| Retained::into_raw(make(Repr::Catalog { color, alpha: None })) as usize)
            .collect()
    });
    // SAFETY: the instances are never released; retaining one gives the
    // caller its own reference.
    unsafe { Retained::retain(all[c as usize] as *mut NSColor) }.expect("a system color")
}

fn imp(c: &NSColor) -> &NSColorImpl {
    // SAFETY: every NSColor is an NSColorImpl (the class has no
    // subclasses in Sidestep, and apps subclassing it inherit its ivars).
    unsafe { &*(c as *const NSColor).cast::<NSColorImpl>() }
}

/// `c` as straight sRGB RGBA, in the current drawing appearance.
pub(crate) fn resolve(c: &NSColor) -> Color {
    resolve_impl(imp(c))
}

thread_local!(static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) });

thread_local!(static EMPHASIS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

/// Run `f` with the catalog colors resolving as they do on an emphasized
/// background (`palette::emphasized`) if `on`, as they don't if not: how
/// the built-in cells style their text for a selected row. Only catalog
/// colors themselves change; colors made from them (a system effect, a
/// dynamic provider's answer, one given another alpha) stay, as on macOS
/// (`conformance/tests/cell_backgrounds.rs`, `emphasized_text`).
pub(crate) fn with_emphasis<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            EMPHASIS.with(|e| e.set(self.0));
        }
    }
    let _restore = Restore(EMPHASIS.with(|e| e.replace(on)));
    f()
}

/// A text field's text color `c` as it draws on an emphasized background:
/// the label color's value, whatever made it (the catalog color, a dynamic
/// provider answering it, its components), becomes the text color for
/// selections; any other color is resolved as [`with_emphasis`] does, so
/// only the catalog colors themselves change. macOS matches the label
/// color by value there, and only there: not in attributed runs, and not
/// the other label colors (`conformance/tests/cell_backgrounds.rs`,
/// `emphasized_text`).
pub(crate) fn resolve_emphasized_text(c: &NSColor) -> Color {
    let look = crate::appearance::current_look();
    let bytes = |v: Color| v.map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8);
    if bytes(with_emphasis(false, || resolve(c))) == bytes(palette::get(System::Label, look)) {
        return palette::get_on(System::Label, look, true);
    }
    with_emphasis(true, || resolve(c))
}

fn resolve_impl(c: &NSColorImpl) -> Color {
    match &c.ivars().repr {
        // Drawing takes what sRGB can show.
        Repr::Components { space, c } => {
            let [r, g, b] = to_srgb(*space, c);
            [r, g, b, c[space.components()]].map(|v| v.clamp(0.0, 1.0) as f32)
        }
        Repr::Catalog { color, alpha } => {
            let emphasized = alpha.is_none() && EMPHASIS.with(std::cell::Cell::get);
            let mut v = palette::get_on(*color, crate::appearance::current_look(), emphasized);
            if let Some(a) = alpha {
                v[3] = *a as f32;
            }
            v
        }
        Repr::Dynamic { provider, alpha, .. } => {
            // A provider that returns itself (or anything dynamic, forever)
            // would never end.
            let depth = DEPTH.with(|d| d.replace(d.get() + 1));
            let mut v = if depth > 8 {
                [0.0, 0.0, 0.0, 1.0]
            } else {
                // The provider, and the color it answers, see no emphasis:
                // macOS doesn't map a label color a provider answers.
                with_emphasis(false, || {
                    let appearance = crate::appearance::get(crate::appearance::current());
                    let got = provider.call((NonNull::from(&*appearance),));
                    // SAFETY: the provider returns a color it keeps alive
                    // (as an autoreleased return value) at least until we
                    // retain it.
                    let color = unsafe { Retained::retain(got.as_ptr()) }.expect("a color");
                    resolve(&color)
                })
            };
            DEPTH.with(|d| d.set(depth));
            if let Some(a) = alpha {
                v[3] = *a as f32;
            }
            v
        }
        Repr::Effect { base, effect } => {
            with_effect(with_emphasis(false, || resolve(base)), *effect, crate::appearance::current_look().dark())
        }
        // Patterns fill with their image's average; tiling comes with
        // pattern paints.
        Repr::Pattern(..) => [0.5, 0.5, 0.5, 1.0],
    }
}

/// `c` as a gradient hands it back: a component color in `space`; a
/// system, dynamic or pattern color as it is.
pub(crate) fn in_space(c: &NSColor, space: Space) -> Retained<NSColor> {
    let this = imp(c);
    match &this.ivars().repr {
        Repr::Components { .. } => convert(this, space).unwrap_or_else(|| c.retain()),
        _ => c.retain(),
    }
}

/// `c` in `space`, clamped to it unless it's an extended one.
fn convert(c: &NSColorImpl, target: Space) -> Option<Retained<NSColor>> {
    match &c.ivars().repr {
        Repr::Pattern(..) => None,
        Repr::Components { space, .. } if *space == target => Some(c.as_color().retain()),
        _ => {
            let (rgb, alpha) = match &c.ivars().repr {
                Repr::Components { space, c } => (to_srgb(*space, c), c[space.components()]),
                _ => {
                    let v = resolve_impl(c).map(f64::from);
                    ([v[0], v[1], v[2]], v[3])
                }
            };
            let comps = from_srgb(target, rgb);
            let mut out = [0.0; 5];
            out[..comps.len()].copy_from_slice(&comps);
            out[comps.len()] = alpha;
            Some(make(Repr::Components { space: target, c: clamp_to(target, out) }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsb_round_trips() {
        for (h, s, b) in [(0.0, 1.0, 1.0), (0.25, 0.5, 0.75), (0.6, 0.3, 0.9), (0.95, 1.0, 0.2)] {
            let [h2, s2, b2] = rgb_to_hsb(hsb_to_rgb(h, s, b));
            assert!((h - h2).abs() < 1e-9 && (s - s2).abs() < 1e-9 && (b - b2).abs() < 1e-9, "{h} {s} {b}");
        }
    }

    #[test]
    fn display_p3_round_trips_through_srgb() {
        let c = [0.2, 0.5, 0.8];
        let back = p3_to_srgb(srgb_to_p3(c));
        assert!(c.iter().zip(back).all(|(a, b)| (a - b).abs() < 1e-4), "{back:?}");
        // P3's red is outside sRGB.
        assert!(p3_to_srgb([1.0, 0.0, 0.0])[0] > 1.0);
    }
}
