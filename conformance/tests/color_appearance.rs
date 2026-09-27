//! Colors, color spaces and appearances, checked on macOS and on Linux
//! alike: constructors and components, derived colors, system colors and
//! how they follow the appearance, dynamic colors, and how views inherit
//! their appearance. No absolute system color is asserted: Sidestep's
//! palette is its own; only the relations programs rely on are.
//!
//! Appearances are always set explicitly, since the Mac running the tests
//! may be in dark mode.
// The old color space names are deprecated, and pinned all the same.
#![allow(deprecated)]

mod common;

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use common::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceName, NSAppearanceNameAccessibilityHighContrastAqua,
    NSAppearanceNameAccessibilityHighContrastDarkAqua, NSAppearanceNameAccessibilityHighContrastVibrantDark,
    NSAppearanceNameAccessibilityHighContrastVibrantLight, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSAppearanceNameVibrantDark, NSAppearanceNameVibrantLight, NSApplication, NSBackgroundStyle,
    NSCalibratedBlackColorSpace, NSCalibratedRGBColorSpace, NSCalibratedWhiteColorSpace, NSColor, NSColorSpace,
    NSColorSpaceModel, NSColorSystemEffect, NSColorType, NSCustomColorSpace, NSDeviceBlackColorSpace,
    NSDeviceCMYKColorSpace, NSDeviceRGBColorSpace, NSDeviceWhiteColorSpace, NSNamedColorSpace, NSPatternColorSpace,
    NSRectFill, NSResponder, NSSystemColorsDidChangeNotification, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString};

use sidestep as _;

fn appearance(name: &NSAppearanceName) -> Retained<NSAppearance> {
    NSAppearance::appearanceNamed(name).expect("a named appearance")
}

fn aqua() -> Retained<NSAppearance> {
    appearance(unsafe { NSAppearanceNameAqua })
}

fn dark() -> Retained<NSAppearance> {
    appearance(unsafe { NSAppearanceNameDarkAqua })
}

/// Run `f` with `a` as the current drawing appearance.
fn within<R>(a: &NSAppearance, f: impl FnOnce() -> R) -> R {
    let out = RefCell::new(None);
    let f = RefCell::new(Some(f));
    let block = RcBlock::new(|| {
        let f = f.borrow_mut().take().expect("called once");
        *out.borrow_mut() = Some(f());
    });
    a.performAsCurrentDrawingAppearance(&block);
    drop(block);
    out.into_inner().expect("the block ran")
}

/// `c` in sRGB: red, green, blue, alpha.
fn srgb(c: &NSColor) -> [f64; 4] {
    let c = c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()).expect("an sRGB color");
    [c.redComponent(), c.greenComponent(), c.blueComponent(), c.alphaComponent()]
}

fn luminance(c: [f64; 4]) -> f64 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// `c` over `bg`, opaque.
fn over(c: [f64; 4], bg: [f64; 4]) -> [f64; 4] {
    [0, 1, 2, 3].map(|i| if i == 3 { 1.0 } else { c[i] * c[3] + bg[i] * (1.0 - c[3]) })
}

fn exported_names(_: MainThreadMarker) {
    let s = |v: &NSString| v.to_string();
    // SAFETY: these are constant strings.
    unsafe {
        assert_eq!(s(NSCalibratedRGBColorSpace), "NSCalibratedRGBColorSpace");
        assert_eq!(s(NSDeviceRGBColorSpace), "NSDeviceRGBColorSpace");
        assert_eq!(s(NSCalibratedWhiteColorSpace), "NSCalibratedWhiteColorSpace");
        assert_eq!(s(NSDeviceWhiteColorSpace), "NSDeviceWhiteColorSpace");
        assert_eq!(s(NSDeviceCMYKColorSpace), "NSDeviceCMYKColorSpace");
        assert_eq!(s(NSNamedColorSpace), "NSNamedColorSpace");
        assert_eq!(s(NSPatternColorSpace), "NSPatternColorSpace");
        assert_eq!(s(NSCustomColorSpace), "NSCustomColorSpace");
        assert_eq!(s(NSCalibratedBlackColorSpace), "NSCalibratedBlackColorSpace");
        assert_eq!(s(NSDeviceBlackColorSpace), "NSDeviceBlackColorSpace");
        assert_eq!(s(NSSystemColorsDidChangeNotification), "NSSystemColorsDidChangeNotification");
        assert_eq!(s(NSAppearanceNameAqua), "NSAppearanceNameAqua");
        assert_eq!(s(NSAppearanceNameDarkAqua), "NSAppearanceNameDarkAqua");
        assert_eq!(s(NSAppearanceNameVibrantLight), "NSAppearanceNameVibrantLight");
        assert_eq!(s(NSAppearanceNameVibrantDark), "NSAppearanceNameVibrantDark");
        assert_eq!(s(NSAppearanceNameAccessibilityHighContrastAqua), "NSAppearanceNameAccessibilityAqua");
        assert_eq!(s(NSAppearanceNameAccessibilityHighContrastDarkAqua), "NSAppearanceNameAccessibilityDarkAqua");
        assert_eq!(
            s(NSAppearanceNameAccessibilityHighContrastVibrantLight),
            "NSAppearanceNameAccessibilityVibrantLight"
        );
        assert_eq!(s(NSAppearanceNameAccessibilityHighContrastVibrantDark), "NSAppearanceNameAccessibilityVibrantDark");
    }
}

fn component_colors(_: MainThreadMarker) {
    let c = NSColor::colorWithSRGBRed_green_blue_alpha(0.1, 0.2, 0.3, 0.4);
    assert_eq!((c.redComponent(), c.greenComponent(), c.blueComponent(), c.alphaComponent()), (0.1, 0.2, 0.3, 0.4));
    assert!(c.colorSpace().isEqual(Some(&NSColorSpace::sRGBColorSpace())));
    assert_eq!(c.r#type(), NSColorType::ComponentBased);
    assert_eq!(c.numberOfComponents(), 4);
    let same = NSColor::colorWithSRGBRed_green_blue_alpha(0.1, 0.2, 0.3, 0.4);
    assert!(c.isEqual(Some(&same)) && c.hash() == same.hash());
    assert!(!c.isEqual(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(0.1, 0.2, 0.3, 0.5))));
    let faded = c.colorWithAlphaComponent(0.9);
    assert_eq!(
        (faded.redComponent(), faded.greenComponent(), faded.blueComponent(), faded.alphaComponent()),
        (0.1, 0.2, 0.3, 0.9)
    );
    let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
    // SAFETY: four writable components.
    unsafe { c.getRed_green_blue_alpha(&mut r, &mut g, &mut b, &mut a) };
    assert_eq!((r, g, b, a), (0.1, 0.2, 0.3, 0.4));

    for make in [
        NSColor::colorWithHue_saturation_brightness_alpha,
        NSColor::colorWithCalibratedHue_saturation_brightness_alpha,
        NSColor::colorWithDeviceHue_saturation_brightness_alpha,
    ] {
        let h = make(0.6, 0.5, 0.8, 1.0);
        let close = |x: f64, y: f64| (x - y).abs() < 1e-6;
        assert!(
            close(h.hueComponent(), 0.6) && close(h.saturationComponent(), 0.5) && close(h.brightnessComponent(), 0.8)
        );
    }
    // Spaces the constructors pick.
    let name = |c: &NSColor| c.colorSpaceName().to_string();
    assert_eq!(
        name(&NSColor::colorWithCalibratedRed_green_blue_alpha(0.1, 0.2, 0.3, 1.0)),
        "NSCalibratedRGBColorSpace"
    );
    assert_eq!(name(&NSColor::colorWithDeviceRed_green_blue_alpha(0.1, 0.2, 0.3, 1.0)), "NSDeviceRGBColorSpace");
    assert_eq!(name(&NSColor::colorWithCalibratedWhite_alpha(0.5, 1.0)), "NSCalibratedWhiteColorSpace");
    assert_eq!(name(&NSColor::colorWithDeviceWhite_alpha(0.5, 1.0)), "NSDeviceWhiteColorSpace");
    assert_eq!(
        name(&NSColor::colorWithDeviceCyan_magenta_yellow_black_alpha(0.1, 0.2, 0.3, 0.4, 1.0)),
        "NSDeviceCMYKColorSpace"
    );
    assert_eq!(
        NSColor::colorWithDeviceCyan_magenta_yellow_black_alpha(0.1, 0.2, 0.3, 0.4, 1.0).numberOfComponents(),
        5
    );
    assert_eq!(name(&NSColor::colorWithSRGBRed_green_blue_alpha(0.1, 0.2, 0.3, 1.0)), "NSCustomColorSpace");

    let w = NSColor::colorWithWhite_alpha(0.25, 1.0);
    assert_eq!(w.colorSpace().colorSpaceModel(), NSColorSpaceModel::Gray);
    assert_eq!((w.whiteComponent(), w.numberOfComponents()), (0.25, 2));
    for c in [NSColor::blackColor(), NSColor::whiteColor(), NSColor::clearColor()] {
        assert_eq!(c.r#type(), NSColorType::ComponentBased);
        assert_eq!(c.colorSpace().colorSpaceModel(), NSColorSpaceModel::Gray);
    }
    assert_eq!(NSColor::clearColor().alphaComponent(), 0.0);
    assert_eq!(NSColor::redColor().colorSpace().colorSpaceModel(), NSColorSpaceModel::RGB);

    // Components beyond 0 to 1 are kept; colorWithRed: puts such a color
    // in extended sRGB.
    let wide = NSColor::colorWithSRGBRed_green_blue_alpha(1.5, -0.5, 0.5, 2.0);
    assert_eq!(
        (wide.redComponent(), wide.greenComponent(), wide.blueComponent(), wide.alphaComponent()),
        (1.5, -0.5, 0.5, 2.0)
    );
    let srgb_space = NSColorSpace::sRGBColorSpace();
    let extended = NSColor::colorWithRed_green_blue_alpha(1.5, -0.5, 0.5, 1.0);
    assert!(extended.colorSpace().isEqual(Some(&NSColorSpace::extendedSRGBColorSpace())));
    assert_eq!((extended.redComponent(), extended.greenComponent()), (1.5, -0.5));
    let plain = NSColor::colorWithRed_green_blue_alpha(0.5, 0.5, 0.5, 1.0);
    assert!(plain.colorSpace().isEqual(Some(&srgb_space)));
    // Into sRGB, a wider color is clamped to it.
    let p3 = NSColor::colorWithDisplayP3Red_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    let in_srgb = p3.colorUsingColorSpace(&srgb_space).expect("sRGB");
    assert!((in_srgb.redComponent() - 1.0).abs() < 1e-6 && in_srgb.greenComponent().abs() < 1e-6);
    assert!((0.0..=1.0).contains(&in_srgb.blueComponent()));
    // RGB to gray weighs light, not encoded values: red is a middle gray,
    // blue a dark one.
    let gray = NSColorSpace::genericGamma22GrayColorSpace();
    let white_of = |c: Retained<NSColor>| c.colorUsingColorSpace(&gray).expect("gray").whiteComponent();
    let (r, b) = (
        white_of(NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0)),
        white_of(NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0)),
    );
    assert!((0.45..0.55).contains(&r), "red {r}");
    assert!((0.22..0.36).contains(&b), "blue {b}");
}

fn blending(_: MainThreadMarker) {
    let black = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 1.0);
    let white = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 1.0);
    let mid = black.blendedColorWithFraction_ofColor(0.5, &white).expect("a blend");
    assert!((mid.redComponent() - 0.5).abs() < 1e-6 && mid.alphaComponent() == 1.0);
    let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    // Toward white, toward black.
    let light = srgb(&red.highlightWithLevel(0.5).unwrap());
    assert!(light[1] > 0.2 && light[0] > 0.75, "{light:?}");
    let shade = srgb(&red.shadowWithLevel(0.5).unwrap());
    assert!(shade[0] < 0.65 && shade[0] > 0.3 && shade[1] < 0.1, "{shade:?}");
}

fn color_spaces(_: MainThreadMarker) {
    assert!(std::ptr::eq(&*NSColorSpace::sRGBColorSpace(), &*NSColorSpace::sRGBColorSpace()), "shared");
    for (space, model, n) in [
        (NSColorSpace::sRGBColorSpace(), NSColorSpaceModel::RGB, 3),
        (NSColorSpace::genericGrayColorSpace(), NSColorSpaceModel::Gray, 1),
        (NSColorSpace::deviceRGBColorSpace(), NSColorSpaceModel::RGB, 3),
        (NSColorSpace::displayP3ColorSpace(), NSColorSpaceModel::RGB, 3),
        (NSColorSpace::genericCMYKColorSpace(), NSColorSpaceModel::CMYK, 4),
    ] {
        assert_eq!((space.colorSpaceModel(), space.numberOfColorComponents()), (model, n));
    }
    let c = NSColor::colorWithDeviceRed_green_blue_alpha(0.1, 0.2, 0.3, 1.0);
    let again = c.colorUsingColorSpace(&NSColorSpace::deviceRGBColorSpace()).unwrap();
    assert!(again.isEqual(Some(&c)), "into its own space: equal");
}

fn system_colors(_: MainThreadMarker) {
    let label = NSColor::labelColor();
    assert_eq!(label.r#type(), NSColorType::Catalog);
    assert!(std::ptr::eq(&*label, &*NSColor::labelColor()), "one shared instance");
    assert_eq!(label.colorNameComponent().to_string(), "labelColor");
    assert_eq!(label.colorSpaceName().to_string(), "NSNamedColorSpace");
    for (a, is_dark) in [(aqua(), false), (dark(), true)] {
        within(&a, || {
            let bg = srgb(&NSColor::windowBackgroundColor());
            assert_eq!(luminance(bg) < 0.5, is_dark, "window background, dark {is_dark}");
            assert_eq!(luminance(srgb(&NSColor::controlBackgroundColor())) < 0.5, is_dark);
            for c in [NSColor::labelColor(), NSColor::textColor()] {
                assert_eq!(luminance(over(srgb(&c), bg)) < 0.5, !is_dark, "text, dark {is_dark}");
            }
            // The label hierarchy fades.
            let contrast: Vec<f64> = [
                NSColor::labelColor(),
                NSColor::secondaryLabelColor(),
                NSColor::tertiaryLabelColor(),
                NSColor::quaternaryLabelColor(),
            ]
            .iter()
            .map(|c| (luminance(over(srgb(c), bg)) - luminance(bg)).abs())
            .collect();
            assert!(contrast.windows(2).all(|w| w[0] >= w[1]), "{contrast:?}");
            // Another alpha replaces the label's.
            let half = NSColor::labelColor().colorWithAlphaComponent(0.5);
            assert!((srgb(&half)[3] - 0.5).abs() < 1e-6);
        });
    }
    // Another alpha resolves the label in the appearance of the moment:
    // the result is a plain color, the same in any appearance after.
    let half = within(&aqua(), || NSColor::labelColor().colorWithAlphaComponent(0.5));
    let (l, d) = (within(&aqua(), || srgb(&half)), within(&dark(), || srgb(&half)));
    assert!(luminance(l) < 0.5 && luminance(d) < 0.5, "{l:?} {d:?}");
    assert_eq!(half.r#type(), NSColorType::ComponentBased);
    let arr = NSColor::alternatingContentBackgroundColors();
    assert!(arr.count() >= 2);
    // A system effect on a system color keeps following the appearance.
    let pressed = within(&aqua(), || NSColor::labelColor().colorWithSystemEffect(NSColorSystemEffect::Pressed));
    assert_eq!(pressed.r#type(), NSColorType::Catalog);
    for (a, is_dark) in [(aqua(), false), (dark(), true)] {
        within(&a, || {
            let bg = srgb(&NSColor::windowBackgroundColor());
            let c = srgb(&pressed);
            assert_eq!(luminance(over(c, bg)) < 0.5, !is_dark, "pressed label, dark {is_dark}: {c:?}");
        });
    }
}

fn system_effects(_: MainThreadMarker) {
    use NSColorSystemEffect as E;
    let rgb = |r: f64, g: f64, b: f64, a: f64| NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a);
    // In 255ths, as macOS gives them. Pressed, deep-pressed and rollover
    // darken in the light appearance and lighten in the dark one; disabled
    // fades (to 35% of the alpha in light, half in dark).
    let cases = [
        (E::Pressed, [0.0, 0.0, 255.0, 255.0], [0.0, 0.0, 215.0, 255.0], [46.0, 46.0, 255.0, 255.0]),
        (E::Pressed, [153.0, 102.0, 51.0, 255.0], [121.0, 74.0, 27.0, 255.0], [199.0, 148.0, 97.0, 255.0]),
        (E::Pressed, [128.0, 128.0, 128.0, 255.0], [98.0, 98.0, 98.0, 255.0], [174.0, 174.0, 174.0, 255.0]),
        (E::DeepPressed, [153.0, 102.0, 51.0, 255.0], [95.0, 52.0, 8.0, 255.0], [235.0, 184.0, 133.0, 255.0]),
        (E::Rollover, [153.0, 102.0, 51.0, 255.0], [95.0, 52.0, 8.0, 255.0], [214.0, 163.0, 112.0, 255.0]),
        (E::Disabled, [153.0, 102.0, 51.0, 255.0], [153.0, 102.0, 51.0, 89.25], [153.0, 102.0, 51.0, 127.5]),
        (E::None, [153.0, 102.0, 51.0, 255.0], [153.0, 102.0, 51.0, 255.0], [153.0, 102.0, 51.0, 255.0]),
        // A translucent color: the step goes to its premultiplied
        // components and its alpha.
        (E::Pressed, [0.0, 0.0, 0.0, 127.5], [0.0, 0.0, 0.0, 158.0], [46.0, 46.0, 46.0, 174.0]),
        (E::Pressed, [255.0, 255.0, 255.0, 127.5], [118.0, 118.0, 118.0, 158.0], [174.0, 174.0, 174.0, 174.0]),
        (E::Pressed, [51.0, 51.0, 51.0, 204.0], [38.0, 38.0, 38.0, 228.0], [87.0, 87.0, 87.0, 250.0]),
        (E::DeepPressed, [0.0, 0.0, 0.0, 127.5], [0.0, 0.0, 0.0, 182.0], [82.0, 82.0, 82.0, 210.0]),
        (E::DeepPressed, [51.0, 51.0, 51.0, 204.0], [35.0, 35.0, 35.0, 247.0], [123.0, 123.0, 123.0, 255.0]),
        (E::Rollover, [0.0, 0.0, 0.0, 127.5], [0.0, 0.0, 0.0, 182.0], [61.0, 61.0, 61.0, 189.0]),
        (E::Disabled, [51.0, 51.0, 51.0, 204.0], [51.0, 51.0, 51.0, 71.4], [51.0, 51.0, 51.0, 102.0]),
    ];
    for (effect, from, light, dark_want) in cases {
        let c = rgb(from[0] / 255.0, from[1] / 255.0, from[2] / 255.0, from[3] / 255.0);
        let e = c.colorWithSystemEffect(effect);
        // Made once, it follows the appearance it's used in.
        assert_eq!(e.r#type(), NSColorType::Catalog, "{effect:?}");
        for (a, want) in [(aqua(), light), (dark(), dark_want)] {
            let got = within(&a, || srgb(&e)).map(|v| v * 255.0);
            let close = got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.51);
            assert!(close, "{effect:?} of {from:?} in {}: {got:?}, not {want:?}", a.name());
        }
    }
    // A system color's too.
    let pressed = NSColor::labelColor().colorWithSystemEffect(E::Pressed);
    let got = within(&dark(), || srgb(&pressed)).map(|v| (v * 255.0).round());
    assert_eq!(got, [255.0; 4]);
}

fn dynamic_colors(mtm: MainThreadMarker) {
    let calls: Rc<RefCell<Vec<String>>> = Rc::default();
    let c2 = calls.clone();
    let provider = RcBlock::new(move |a: NonNull<NSAppearance>| -> NonNull<NSColor> {
        // SAFETY: AppKit passes an appearance.
        let a = unsafe { a.as_ref() };
        c2.borrow_mut().push(a.name().to_string());
        let names = NSArray::from_slice(&[unsafe { NSAppearanceNameAqua }, unsafe { NSAppearanceNameDarkAqua }]);
        let is_dark = a.bestMatchFromAppearancesWithNames(&names).is_some_and(|n| n.to_string().contains("Dark"));
        let c = if is_dark { NSColor::whiteColor() } else { NSColor::blackColor() };
        // Returned autoreleased, as an Objective-C block would.
        NonNull::new(Retained::autorelease_return(c)).expect("a color")
    });
    // SAFETY: the provider returns a color for every appearance.
    let dynamic = unsafe { NSColor::colorWithName_dynamicProvider(None, &provider) };
    assert!(calls.borrow().is_empty(), "not called when made");
    assert_eq!(dynamic.r#type(), NSColorType::Catalog);
    let in_dark = within(&dark(), || srgb(&dynamic));
    assert_eq!(calls.borrow().last().map(String::as_str), Some("NSAppearanceNameDarkAqua"));
    assert!(luminance(in_dark) > 0.5);
    let in_light = within(&aqua(), || srgb(&dynamic));
    assert!(luminance(in_light) < 0.5);
    // Given another alpha, a dynamic color stays dynamic.
    let faded = within(&aqua(), || dynamic.colorWithAlphaComponent(0.5));
    assert_eq!(faded.r#type(), NSColorType::Catalog);
    let (d, l) = (within(&dark(), || srgb(&faded)), within(&aqua(), || srgb(&faded)));
    assert!(luminance(d) > 0.5 && luminance(l) < 0.5 && (d[3] - 0.5).abs() < 1e-6, "{d:?} {l:?}");

    // Two sibling views, one dark, one light, drawing the same color.
    let parent = draw_view(mtm, rect(0.0, 0.0, 8.0, 4.0), false, |_, _| {});
    let left = {
        let dynamic = dynamic.clone();
        draw_view(mtm, rect(0.0, 0.0, 4.0, 4.0), false, move |_, _| {
            dynamic.setFill();
            NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
        })
    };
    let right = {
        let dynamic = dynamic.clone();
        draw_view(mtm, rect(4.0, 0.0, 4.0, 4.0), false, move |_, _| {
            dynamic.setFill();
            NSRectFill(rect(0.0, 0.0, 4.0, 4.0));
        })
    };
    left.setAppearance(Some(&dark()));
    right.setAppearance(Some(&aqua()));
    parent.addSubview(&left);
    parent.addSubview(&right);
    let rep = snapshot(&parent, 1.0);
    assert_px(&rep, 1, 1, WHITE);
    assert_px(&rep, 6, 1, BLACK);
}

fn named_appearances(_: MainThreadMarker) {
    // SAFETY: constant strings.
    let names = unsafe {
        [
            (NSAppearanceNameAqua, "NSAppearanceNameAqua", false, Some(0)),
            (NSAppearanceNameDarkAqua, "NSAppearanceNameDarkAqua", false, Some(1)),
            (NSAppearanceNameVibrantLight, "NSAppearanceNameVibrantLight", true, Some(0)),
            (NSAppearanceNameVibrantDark, "NSAppearanceNameVibrantDark", true, Some(1)),
            (NSAppearanceNameAccessibilityHighContrastAqua, "NSAppearanceNameAqua", false, Some(0)),
            (NSAppearanceNameAccessibilityHighContrastDarkAqua, "NSAppearanceNameDarkAqua", false, Some(1)),
            (NSAppearanceNameAccessibilityHighContrastVibrantLight, "NSAppearanceNameVibrantLight", true, Some(0)),
            (NSAppearanceNameAccessibilityHighContrastVibrantDark, "NSAppearanceNameVibrantDark", true, Some(1)),
        ]
    };
    // SAFETY: constant strings.
    let (aqua_name, dark_name) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    let both = NSArray::from_slice(&[aqua_name, dark_name]);
    let only_aqua = NSArray::from_slice(&[aqua_name]);
    for (name, reported, vibrant, best) in names {
        let a = appearance(name);
        assert!(std::ptr::eq(&*a, &*appearance(name)), "{name}: shared");
        // High-contrast appearances report their base appearance's name.
        assert_eq!(a.name().to_string(), reported);
        assert_eq!(a.allowsVibrancy(), vibrant, "{name}");
        let got = a.bestMatchFromAppearancesWithNames(&both).map(|n| n.to_string());
        assert_eq!(got.as_deref(), best.map(|i| ["NSAppearanceNameAqua", "NSAppearanceNameDarkAqua"][i]), "{name}");
        let got = a.bestMatchFromAppearancesWithNames(&only_aqua).map(|n| n.to_string());
        assert_eq!(got.is_some(), best == Some(0), "{name} against Aqua alone");
    }
}

fn drawing_appearance(mtm: MainThreadMarker) {
    // performAsCurrentDrawingAppearance: nests and restores.
    let outer = within(&dark(), || {
        let inner = within(&aqua(), || NSAppearance::currentDrawingAppearance().name().to_string());
        (inner, NSAppearance::currentDrawingAppearance().name().to_string())
    });
    assert_eq!(outer, ("NSAppearanceNameAqua".into(), "NSAppearanceNameDarkAqua".into()));
    // The application's own appearance. (What currentDrawingAppearance and a
    // view outside any window report here follows the system's light or
    // dark setting on macOS, not only the application's, so they aren't
    // pinned: a dark Mac and CI's light one disagree.)
    let app = NSApplication::sharedApplication(mtm);
    app.setAppearance(Some(&dark()));
    assert_eq!(app.effectiveAppearance().name().to_string(), "NSAppearanceNameDarkAqua");
    let _ = draw_view(mtm, rect(0.0, 0.0, 1.0, 1.0), false, |_, _| {});
    app.setAppearance(None);
}

// A view counting viewDidChangeEffectiveAppearance.
define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceAppearanceView"]
    #[ivars = Cell<usize>]
    struct Counting;

    impl Counting {
        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn changed(&self) {
            self.ivars().set(self.ivars().get() + 1);
        }
    }
);

fn counting(mtm: MainThreadMarker) -> Retained<Counting> {
    let this = Counting::alloc(mtm).set_ivars(Cell::new(0));
    // SAFETY: NSView's designated initializer.
    unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 10.0, 10.0)] }
}

fn inheritance(mtm: MainThreadMarker) {
    let parent = draw_view(mtm, rect(0.0, 0.0, 10.0, 10.0), false, |_, _| {});
    let child = counting(mtm);
    parent.setAppearance(Some(&dark()));
    parent.addSubview(&child);
    assert_eq!(child.effectiveAppearance().name().to_string(), "NSAppearanceNameDarkAqua", "from the superview");
    let before = child.ivars().get();
    parent.setAppearance(Some(&aqua()));
    assert_eq!(child.effectiveAppearance().name().to_string(), "NSAppearanceNameAqua");
    assert_eq!(child.ivars().get(), before + 1, "told at once");
    parent.setAppearance(Some(&aqua()));
    assert_eq!(child.ivars().get(), before + 1, "not told when nothing changed");
    child.setAppearance(Some(&dark()));
    assert_eq!(child.effectiveAppearance().name().to_string(), "NSAppearanceNameDarkAqua", "its own first");
    child.setAppearance(None);
    assert_eq!(child.effectiveAppearance().name().to_string(), "NSAppearanceNameAqua", "nil inherits again");
    assert!(child.appearance().is_none());
    // Moving under a parent of another appearance.
    let other = draw_view(mtm, rect(0.0, 0.0, 10.0, 10.0), false, |_, _| {});
    other.setAppearance(Some(&dark()));
    let told = child.ivars().get();
    other.addSubview(&child);
    assert_eq!(child.effectiveAppearance().name().to_string(), "NSAppearanceNameDarkAqua");
    assert_eq!(child.ivars().get(), told + 1);

    // In drawRect:, the drawing appearance is the view's.
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let s = seen.clone();
    let view = draw_view(mtm, rect(0.0, 0.0, 2.0, 2.0), false, move |v, _| {
        let current = NSAppearance::currentDrawingAppearance().name().to_string();
        s.borrow_mut().push(format!("{current} {}", v.effectiveAppearance().name()));
    });
    for a in [dark(), aqua()] {
        view.setAppearance(Some(&a));
        snapshot(&view, 1.0);
    }
    assert_eq!(
        *seen.borrow(),
        ["NSAppearanceNameDarkAqua NSAppearanceNameDarkAqua", "NSAppearanceNameAqua NSAppearanceNameAqua"]
    );
    let _: Option<&AnyObject> = None;
}

fn visual_effect_views(mtm: MainThreadMarker) {
    let v = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), common::rect(0.0, 0.0, 4.0, 4.0));
    assert_eq!(v.material(), NSVisualEffectMaterial::AppearanceBased);
    assert_eq!(v.blendingMode(), NSVisualEffectBlendingMode::BehindWindow);
    assert_eq!(v.state(), NSVisualEffectState::FollowsWindowActiveState);
    assert!(!v.isEmphasized() && v.maskImage().is_none());
    assert_eq!(v.interiorBackgroundStyle(), NSBackgroundStyle::Normal);
    v.setBlendingMode(NSVisualEffectBlendingMode::WithinWindow);
    v.setState(NSVisualEffectState::Active);
    assert_eq!((v.blendingMode(), v.state()), (NSVisualEffectBlendingMode::WithinWindow, NSVisualEffectState::Active));
    // A selection that's emphasized carries emphasized content.
    v.setMaterial(NSVisualEffectMaterial::Selection);
    v.setEmphasized(true);
    assert_eq!(v.interiorBackgroundStyle(), NSBackgroundStyle::Emphasized);
    // A sidebar draws opaque, light or dark as its appearance.
    v.setMaterial(NSVisualEffectMaterial::Sidebar);
    v.setEmphasized(false);
    for (a, is_dark) in [(aqua(), false), (dark(), true)] {
        v.setAppearance(Some(&a));
        let p = pixel(&snapshot(&v, 1.0), 1, 1);
        let luminance = (0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])) / 255.0;
        assert_eq!((p[3], luminance < 0.5), (255, is_dark), "{p:?}");
    }
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("exported_names", exported_names),
        ("component_colors", component_colors),
        ("blending", blending),
        ("color_spaces", color_spaces),
        ("system_colors", system_colors),
        ("system_effects", system_effects),
        ("dynamic_colors", dynamic_colors),
        ("named_appearances", named_appearances),
        ("drawing_appearance", drawing_appearance),
        ("inheritance", inheritance),
        ("visual_effect_views", visual_effect_views),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
