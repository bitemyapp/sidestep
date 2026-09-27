//! `NSFont` and `NSFontDescriptor`.
//!
//! A descriptor holds a [`FontSpec`]: a family (or one of the system
//! designs), a weight, italic, a width and a size. A font is a spec
//! resolved to a face (see `text::fonts`), which answers the metrics and
//! names. The system font is the desktop's interface font as fontconfig
//! resolves `system-ui`, or `sans-serif`; the monospaced one is
//! `monospace`. `NSFontWeight` values map through the named weights to CSS
//! weights, so `NSFontWeightSemibold` asks for 600, and a family without
//! that weight gets its nearest face, emboldened when needed.
//!
//! Fonts are immutable and cached per thread, so asking for the same font
//! twice gives the same object, as on macOS.
//!
//! Size 0 means a default, which depends on how the font was asked for, as
//! measured on macOS: 13 points (`systemFontSize`) for the interface fonts
//! (`systemFontOfSize:` and its kin), 12 for fonts found by name or
//! descriptor, for the user fonts and for monospaced ones. A font remembers
//! its default, which `fontWithSize:0` returns to.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSControlSize, NSFont, NSFontDescriptor, NSFontDescriptorSymbolicTraits, NSFontDescriptorSystemDesignMonospaced,
    NSFontDescriptorSystemDesignRounded, NSFontDescriptorSystemDesignSerif, NSFontFamilyAttribute,
    NSFontFeatureSelectorIdentifierKey, NSFontFeatureSettingsAttribute, NSFontFeatureTypeIdentifierKey,
    NSFontNameAttribute, NSFontSizeAttribute, NSFontSymbolicTrait, NSFontTextStyleBody, NSFontTextStyleCallout,
    NSFontTextStyleCaption1, NSFontTextStyleCaption2, NSFontTextStyleFootnote, NSFontTextStyleHeadline,
    NSFontTextStyleLargeTitle, NSFontTextStyleSubheadline, NSFontTextStyleTitle1, NSFontTextStyleTitle2,
    NSFontTextStyleTitle3, NSFontTraitsAttribute, NSFontVisibleNameAttribute, NSFontWeightTrait, NSFontWidthTrait,
    NSGlyph,
};
use objc2_foundation::{NSCopying, NSDictionary, NSMutableCopying, NSPoint, NSRect, NSSize, NSString, NSZone};

use crate::coretext::any;
use crate::text::fonts::{self, Design, Face, Family, FontSpec};
use crate::text::layout::TextFont;

/// Point sizes AppKit uses when asked for size 0.
const SYSTEM_SIZE: f64 = 13.0;
const SMALL_SIZE: f64 = 11.0;
const LABEL_SIZE: f64 = 10.0;
const USER_SIZE: f64 = 12.0;
const USER_FIXED_SIZE: f64 = 11.0;

pub(crate) struct FontIvars {
    spec: FontSpec,
    face: Arc<Face>,
    /// The size `fontWithSize:` gives for 0.
    zero: f64,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSFont"]
    #[ivars = FontIvars]
    pub(crate) struct NSFontImpl;

    impl NSFontImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let spec = FontSpec::system(Design::Default, USER_SIZE);
            let this = this.set_ivars(FontIvars { face: fonts::resolve(&spec), spec, zero: USER_SIZE });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(systemFontOfSize:))]
        fn system(size: f64) -> Retained<Self> {
            interface(FontSpec::system(Design::Default, or(size, SYSTEM_SIZE)))
        }

        #[unsafe(method_id(boldSystemFontOfSize:))]
        fn bold_system(size: f64) -> Retained<Self> {
            interface(FontSpec { weight: 700.0, ..FontSpec::system(Design::Default, or(size, SYSTEM_SIZE)) })
        }

        #[unsafe(method_id(systemFontOfSize:weight:))]
        fn system_weight(size: f64, weight: f64) -> Retained<Self> {
            interface(FontSpec {
                weight: fonts::css_weight(weight),
                ..FontSpec::system(Design::Default, or(size, SYSTEM_SIZE))
            })
        }

        #[unsafe(method_id(systemFontOfSize:weight:width:))]
        fn system_weight_width(size: f64, weight: f64, width: f64) -> Retained<Self> {
            interface(FontSpec {
                weight: fonts::css_weight(weight),
                stretch: stretch_of_width(width),
                ..FontSpec::system(Design::Default, or(size, SYSTEM_SIZE))
            })
        }

        #[unsafe(method_id(monospacedSystemFontOfSize:weight:))]
        fn monospaced_system(size: f64, weight: f64) -> Retained<Self> {
            font(FontSpec { weight: fonts::css_weight(weight), ..FontSpec::system(Design::Monospaced, or(size, SYSTEM_SIZE)) })
        }

        #[unsafe(method_id(monospacedDigitSystemFontOfSize:weight:))]
        fn monospaced_digit_system(size: f64, weight: f64) -> Retained<Self> {
            font(FontSpec {
                weight: fonts::css_weight(weight),
                tabular_digits: true,
                ..FontSpec::system(Design::Default, or(size, SYSTEM_SIZE))
            })
        }

        #[unsafe(method_id(labelFontOfSize:))]
        fn label(size: f64) -> Retained<Self> {
            interface(FontSpec::system(Design::Default, or(size, LABEL_SIZE)))
        }

        #[unsafe(method_id(messageFontOfSize:))]
        fn message(size: f64) -> Retained<Self> {
            interface(FontSpec::system(Design::Default, or(size, SYSTEM_SIZE)))
        }

        #[unsafe(method_id(controlContentFontOfSize:))]
        fn control_content(size: f64) -> Retained<Self> {
            interface(FontSpec::system(Design::Default, or(size, USER_SIZE)))
        }

        #[unsafe(method_id(menuFontOfSize:))]
        fn menu(size: f64) -> Retained<Self> {
            interface(FontSpec::system(Design::Default, or(size, SYSTEM_SIZE)))
        }

        #[unsafe(method_id(menuBarFontOfSize:))]
        fn menu_bar(size: f64) -> Retained<Self> {
            interface(FontSpec::system(Design::Default, or(size, SYSTEM_SIZE)))
        }

        #[unsafe(method_id(titleBarFontOfSize:))]
        fn title_bar(size: f64) -> Retained<Self> {
            interface(FontSpec { weight: 700.0, ..FontSpec::system(Design::Default, or(size, SYSTEM_SIZE)) })
        }

        #[unsafe(method_id(paletteFontOfSize:))]
        fn palette(size: f64) -> Retained<Self> {
            interface(FontSpec::system(Design::Default, or(size, SMALL_SIZE)))
        }

        #[unsafe(method_id(toolTipsFontOfSize:))]
        fn tool_tips(size: f64) -> Retained<Self> {
            font_with_zero(FontSpec::system(Design::Default, or(size, SMALL_SIZE)), SMALL_SIZE)
        }

        #[unsafe(method_id(userFontOfSize:))]
        fn user(size: f64) -> Option<Retained<Self>> {
            Some(font(FontSpec::system(Design::Default, or(size, USER_SIZE))))
        }

        #[unsafe(method_id(userFixedPitchFontOfSize:))]
        fn user_fixed_pitch(size: f64) -> Option<Retained<Self>> {
            Some(font(FontSpec::system(Design::Monospaced, or(size, USER_FIXED_SIZE))))
        }

        #[unsafe(method(systemFontSize))]
        fn system_font_size() -> f64 {
            SYSTEM_SIZE
        }

        #[unsafe(method(smallSystemFontSize))]
        fn small_system_font_size() -> f64 {
            SMALL_SIZE
        }

        #[unsafe(method(labelFontSize))]
        fn label_font_size() -> f64 {
            LABEL_SIZE
        }

        #[unsafe(method(systemFontSizeForControlSize:))]
        fn system_font_size_for_control_size(size: NSControlSize) -> f64 {
            match size {
                NSControlSize::Small => SMALL_SIZE,
                NSControlSize::Mini => 9.0,
                _ => SYSTEM_SIZE,
            }
        }

        #[unsafe(method_id(preferredFontForTextStyle:options:))]
        fn preferred_for_text_style(style: &NSString, _options: &NSDictionary<NSString, AnyObject>) -> Retained<Self> {
            font(text_style(style))
        }

        #[unsafe(method_id(fontWithName:size:))]
        fn with_name(name: &NSString, size: f64) -> Option<Retained<Self>> {
            fonts::spec_named(&name.to_string(), or(size, USER_SIZE)).map(font)
        }

        #[unsafe(method_id(fontWithDescriptor:size:))]
        fn with_descriptor(descriptor: &NSFontDescriptor, size: f64) -> Option<Retained<Self>> {
            let spec = &descriptor_imp(descriptor).ivars().spec;
            // A descriptor naming a font nobody has makes no font.
            (!spec.missing).then(|| font(FontSpec { size: or(size, or(spec.size, USER_SIZE)), ..spec.clone() }))
        }

        #[unsafe(method_id(fontWithSize:))]
        fn with_size(&self, size: f64) -> Retained<Self> {
            let ivars = self.ivars();
            font_with_zero(FontSpec { size: or(size, ivars.zero), ..ivars.spec.clone() }, ivars.zero)
        }

        #[unsafe(method(pointSize))]
        fn point_size(&self) -> f64 {
            self.ivars().spec.size
        }

        #[unsafe(method_id(fontName))]
        fn font_name(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().face.postscript_name)
        }

        #[unsafe(method_id(familyName))]
        fn family_name(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&self.ivars().face.family_name))
        }

        #[unsafe(method_id(displayName))]
        fn display_name(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(&self.ivars().face.full_name))
        }

        #[unsafe(method_id(fontDescriptor))]
        fn font_descriptor(&self) -> Retained<NSFontDescriptor> {
            descriptor(self.ivars().spec.clone())
        }

        #[unsafe(method(ascender))]
        fn ascender(&self) -> f64 {
            self.in_units(|u| u.ascent, |m| m.ascent)
        }

        #[unsafe(method(descender))]
        fn descender(&self) -> f64 {
            self.in_units(|u| u.descent, |m| m.descent)
        }

        #[unsafe(method(leading))]
        fn leading(&self) -> f64 {
            self.in_units(|u| u.leading, |m| m.leading)
        }

        #[unsafe(method(capHeight))]
        fn cap_height(&self) -> f64 {
            self.in_units(|u| u.cap_height, |m| m.cap_height)
        }

        #[unsafe(method(xHeight))]
        fn x_height(&self) -> f64 {
            self.in_units(|u| u.x_height, |m| m.x_height)
        }

        #[unsafe(method(underlinePosition))]
        fn underline_position(&self) -> f64 {
            self.in_units(|u| u.underline_position, |m| m.underline_position)
        }

        #[unsafe(method(underlineThickness))]
        fn underline_thickness(&self) -> f64 {
            self.in_units(|u| u.underline_thickness, |m| m.underline_thickness)
        }

        #[unsafe(method(italicAngle))]
        fn italic_angle(&self) -> f64 {
            f64::from(self.ivars().face.metrics.italic_angle)
        }

        #[unsafe(method(isFixedPitch))]
        fn is_fixed_pitch(&self) -> bool {
            self.ivars().face.fixed_pitch
        }

        #[unsafe(method(boundingRectForFont))]
        fn bounding_rect_for_font(&self) -> NSRect {
            let [x0, y0, x1, y1] = self.ivars().face.metrics.bounds.map(|v| f64::from(v) * self.ivars().spec.size);
            NSRect::new(NSPoint::new(x0, y0), NSSize::new(x1 - x0, y1 - y0))
        }

        #[unsafe(method(maximumAdvancement))]
        fn maximum_advancement(&self) -> NSSize {
            NSSize::new(self.scaled(|m| m.max_advance), 0.0)
        }

        #[unsafe(method(numberOfGlyphs))]
        fn number_of_glyphs(&self) -> usize {
            self.ivars().face.glyph_count as usize
        }

        #[unsafe(method(advancementForGlyph:))]
        fn advancement_for_glyph(&self, glyph: NSGlyph) -> NSSize {
            NSSize::new(self.glyph_metrics(glyph).0, 0.0)
        }

        #[unsafe(method(boundingRectForGlyph:))]
        fn bounding_rect_for_glyph(&self, glyph: NSGlyph) -> NSRect {
            self.glyph_metrics(glyph).1
        }

        #[unsafe(method(advancementForCGGlyph:))]
        fn advancement_for_cg_glyph(&self, glyph: u16) -> NSSize {
            NSSize::new(self.glyph_metrics(glyph.into()).0, 0.0)
        }

        #[unsafe(method(boundingRectForCGGlyph:))]
        fn bounding_rect_for_cg_glyph(&self, glyph: u16) -> NSRect {
            self.glyph_metrics(glyph.into()).1
        }

        #[unsafe(method(isVertical))]
        fn is_vertical(&self) -> bool {
            false
        }

        #[unsafe(method_id(verticalFont))]
        fn vertical_font(&self) -> Retained<Self> {
            self.retain()
        }

        #[unsafe(method_id(screenFont))]
        fn screen_font(&self) -> Retained<Self> {
            self.retain()
        }

        #[unsafe(method_id(printerFont))]
        fn printer_font(&self) -> Retained<Self> {
            self.retain()
        }

        #[unsafe(method(set))]
        fn set(&self) {}

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut Self {
            // Immutable: a copy is the same font.
            Retained::into_raw(self.retain())
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSFont>()).is_some_and(|o| imp(o).ivars().spec == self.ivars().spec)
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            let spec = &self.ivars().spec;
            (spec.size.to_bits() as usize).rotate_left(7) ^ spec.weight.to_bits() as usize ^ usize::from(spec.italic)
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let ivars = self.ivars();
            NSString::from_str(&format!("\"{}\" {:.2} pt.", ivars.face.postscript_name, ivars.spec.size))
        }
    }

    unsafe impl NSObjectProtocol for NSFontImpl {}
);

impl NSFontImpl {
    fn scaled(&self, f: impl Fn(&fonts::Metrics) -> f32) -> f64 {
        f64::from(f(&self.ivars().face.metrics)) * self.ivars().spec.size
    }

    /// A metric from the font file's units, scaled as CoreText scales them
    /// (so a font's `ascender` and `CTFontGetAscent` agree to the last
    /// bit); from `metrics` for a face without a file.
    fn in_units(&self, units: impl Fn(&fonts::Units) -> f64, metrics: impl Fn(&fonts::Metrics) -> f32) -> f64 {
        let u = &self.ivars().face.units;
        if u.per_em > 0.0 { units(u) * (self.ivars().spec.size / u.per_em) } else { self.scaled(metrics) }
    }

    /// A glyph's advance and bounding box at this size.
    fn glyph_metrics(&self, glyph: NSGlyph) -> (f64, NSRect) {
        let size = self.ivars().spec.size;
        let found = fonts::glyph_metrics(&self.ivars().face, glyph).unwrap_or_default();
        let [x0, y0, x1, y1] = found.1.map(|v| f64::from(v) * size);
        (f64::from(found.0) * size, NSRect::new(NSPoint::new(x0, y0), NSSize::new(x1 - x0, y1 - y0)))
    }
}

/// The fonts of the text styles, with macOS's sizes and weights.
fn text_style(style: &NSString) -> FontSpec {
    // SAFETY: the constants are this crate's own, always valid.
    let styles = unsafe {
        [
            (NSFontTextStyleLargeTitle, 26.0, 400.0),
            (NSFontTextStyleTitle1, 22.0, 400.0),
            (NSFontTextStyleTitle2, 17.0, 400.0),
            (NSFontTextStyleTitle3, 15.0, 400.0),
            (NSFontTextStyleHeadline, 13.0, 700.0),
            (NSFontTextStyleSubheadline, 11.0, 400.0),
            (NSFontTextStyleBody, 13.0, 400.0),
            (NSFontTextStyleCallout, 12.0, 400.0),
            (NSFontTextStyleFootnote, 10.0, 400.0),
            (NSFontTextStyleCaption1, 10.0, 400.0),
            (NSFontTextStyleCaption2, 10.0, 500.0),
        ]
    };
    // A style macOS doesn't know gets its default font, 12-point.
    let (size, weight) = styles.iter().find(|s| s.0 == style).map_or((USER_SIZE, 400.0), |s| (s.1, s.2));
    FontSpec { weight, ..FontSpec::system(Design::Default, size) }
}

fn or(size: f64, default: f64) -> f64 {
    if size > 0.0 { size } else { default }
}

/// `NSFontWidth` (−0.3 to 0.2 for the named widths) as a width ratio.
fn stretch_of_width(width: f64) -> f32 {
    (1.0 + width * 1.25).clamp(0.5, 2.0) as f32
}

pub(crate) fn imp(font: &NSFont) -> &NSFontImpl {
    // SAFETY: every NSFont is an NSFontImpl; the class isn't subclassed.
    unsafe { &*(font as *const NSFont).cast::<NSFontImpl>() }
}

/// The font as text layout uses it.
pub(crate) fn text_font(font: &NSFont) -> TextFont {
    let ivars = imp(font).ivars();
    TextFont {
        face: ivars.face.clone(),
        size: ivars.spec.size as f32,
        tabular_digits: ivars.spec.tabular_digits,
        features: ivars.spec.features.clone(),
    }
}

/// The font `spec` describes, whose size 0 is 12 points.
pub(crate) fn font(spec: FontSpec) -> Retained<NSFontImpl> {
    font_with_zero(spec, USER_SIZE)
}

/// One of the interface fonts, whose size 0 is `systemFontSize`.
fn interface(spec: FontSpec) -> Retained<NSFontImpl> {
    font_with_zero(spec, SYSTEM_SIZE)
}

/// The font `spec` describes, whose size 0 is `zero`, from this thread's
/// cache if it was made before.
fn font_with_zero(spec: FontSpec, zero: f64) -> Retained<NSFontImpl> {
    type Key = (fonts::FaceKey, u64, bool, Option<fonts::Features>, u64);
    thread_local!(static FONTS: RefCell<HashMap<Key, Retained<NSFontImpl>>> = RefCell::default());
    let key = (spec.key(), spec.size.to_bits(), spec.tabular_digits, spec.features.clone(), zero.to_bits());
    if let Some(font) = FONTS.with(|f| f.borrow().get(&key).cloned()) {
        return font;
    }
    let face = fonts::resolve(&spec);
    crate::load_shell::<objc2_app_kit::NSFont>();
    let this = NSFontImpl::alloc().set_ivars(FontIvars { spec, face, zero });
    // SAFETY: NSObject's designated initializer.
    let font: Retained<NSFontImpl> = unsafe { msg_send![super(this), init] };
    FONTS.with(|f| {
        let mut fonts = f.borrow_mut();
        // Programs that make fonts of ever new sizes shouldn't grow this
        // without bound.
        if fonts.len() >= 512 {
            fonts.clear();
        }
        fonts.insert(key, font.clone());
    });
    font
}

/// Make sure a class's shell has loaded before its implementation type is
/// used directly.
fn load<T: ClassType>() {
    // SAFETY: +class takes nothing and returns the receiver.
    let _: &AnyClass = unsafe { msg_send![T::class(), class] };
}

// NSFontDescriptor

pub(crate) struct DescriptorIvars {
    spec: FontSpec,
    /// The attributes the descriptor was made with, which `fontAttributes`
    /// reports as they were given (as on macOS); `None` for one made from
    /// a font or by a derivation that resolves, whose are worked out from
    /// the spec ([`attributes_of`]).
    attributes: Option<Retained<NSDictionary<NSString, AnyObject>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSFontDescriptor"]
    #[ivars = DescriptorIvars]
    pub(crate) struct NSFontDescriptorImpl;

    impl NSFontDescriptorImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let spec = FontSpec::system(Design::Default, 0.0);
            let this = this.set_ivars(DescriptorIvars { spec, attributes: Some(NSDictionary::new()) });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithFontAttributes:))]
        fn init_with_attributes(
            this: Allocated<Self>,
            attributes: Option<&NSDictionary<NSString, AnyObject>>,
        ) -> Retained<Self> {
            let spec = spec_of_attributes(attributes);
            let attributes = Some(attributes.map_or_else(NSDictionary::new, |a| a.copy()));
            let this = this.set_ivars(DescriptorIvars { spec, attributes });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(fontDescriptorWithFontAttributes:))]
        fn with_attributes(attributes: Option<&NSDictionary<NSString, AnyObject>>) -> Retained<Self> {
            let given = attributes.map_or_else(NSDictionary::new, |a| a.copy());
            new_descriptor_with(spec_of_attributes(attributes), Some(given))
        }

        #[unsafe(method_id(preferredFontDescriptorForTextStyle:options:))]
        fn preferred_for_text_style(style: &NSString, _options: &NSDictionary<NSString, AnyObject>) -> Retained<Self> {
            new_descriptor(text_style(style))
        }

        #[unsafe(method_id(fontDescriptorWithName:size:))]
        fn with_name_size(name: &NSString, size: f64) -> Retained<Self> {
            // SAFETY: the constants are this crate's own, always valid.
            let keys = unsafe { [NSFontNameAttribute, NSFontSizeAttribute] };
            let size_number = objc2_foundation::NSNumber::new_f64(size);
            let given = NSDictionary::from_slices(&keys, &[name as &AnyObject, &*size_number as &AnyObject]);
            new_descriptor_with(spec_of_name(&name.to_string(), size), Some(given))
        }

        #[unsafe(method_id(postscriptName))]
        fn postscript_name(&self) -> Option<Retained<NSString>> {
            let spec = &self.ivars().spec;
            match &spec.family {
                // The name asked for, though no font has it, as on macOS.
                Family::Named(name) if spec.missing => Some(NSString::from_str(name)),
                _ => Some(NSString::from_str(&fonts::resolve(spec).postscript_name)),
            }
        }

        #[unsafe(method(pointSize))]
        fn point_size(&self) -> f64 {
            self.ivars().spec.size
        }

        #[unsafe(method(symbolicTraits))]
        fn symbolic_traits(&self) -> NSFontDescriptorSymbolicTraits {
            traits_of(&self.ivars().spec)
        }

        #[unsafe(method_id(fontDescriptorWithSymbolicTraits:))]
        fn with_symbolic_traits(&self, traits: NSFontDescriptorSymbolicTraits) -> Retained<Self> {
            let mut spec = self.ivars().spec.clone();
            let bold = traits.contains(NSFontDescriptorSymbolicTraits::TraitBold);
            if bold != (spec.weight >= 600.0) {
                spec.weight = if bold { 700.0 } else { 400.0 };
            }
            spec.italic = traits.contains(NSFontDescriptorSymbolicTraits::TraitItalic);
            spec.stretch = if traits.contains(NSFontDescriptorSymbolicTraits::TraitCondensed) {
                0.75
            } else if traits.contains(NSFontDescriptorSymbolicTraits::TraitExpanded) {
                1.25
            } else {
                1.0
            };
            if traits.contains(NSFontDescriptorSymbolicTraits::TraitMonoSpace) && !fonts::resolve(&spec).fixed_pitch {
                spec.family = Family::System(Design::Monospaced);
            }
            new_descriptor(spec)
        }

        #[unsafe(method_id(fontDescriptorByAddingAttributes:))]
        fn by_adding_attributes(&self, attributes: &NSDictionary<NSString, AnyObject>) -> Retained<Self> {
            let mut spec = self.ivars().spec.clone();
            apply_attributes(&mut spec, attributes);
            new_descriptor_with(spec, Some(merged(&self.attributes(), attributes)))
        }

        #[unsafe(method_id(fontDescriptorWithSize:))]
        fn with_size(&self, size: f64) -> Retained<Self> {
            // SAFETY: the constant is this crate's own, always valid.
            let key = unsafe { NSFontSizeAttribute };
            let number = objc2_foundation::NSNumber::new_f64(size);
            let added = NSDictionary::from_slices(&[key], &[&*number as &AnyObject]);
            new_descriptor_with(FontSpec { size, ..self.ivars().spec.clone() }, Some(merged(&self.attributes(), &added)))
        }

        #[unsafe(method_id(fontDescriptorWithFamily:))]
        fn with_family(&self, family: &NSString) -> Retained<Self> {
            let named = spec_of_name(&family.to_string(), 0.0);
            new_descriptor(FontSpec { family: named.family, missing: named.missing, ..self.ivars().spec.clone() })
        }

        #[unsafe(method_id(fontDescriptorWithFace:))]
        fn with_face(&self, face: &NSString) -> Retained<Self> {
            let mut spec = FontSpec { weight: 400.0, italic: false, stretch: 1.0, ..self.ivars().spec.clone() };
            if fonts::parse_style(&face.to_string(), &mut spec).is_none() {
                spec = self.ivars().spec.clone();
            }
            new_descriptor(spec)
        }

        #[unsafe(method_id(fontDescriptorWithDesign:))]
        fn with_design(&self, design: &NSString) -> Option<Retained<Self>> {
            with_design(&self.ivars().spec, design)
        }

        #[unsafe(method_id(objectForKey:))]
        fn object_for_key(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            let given = self.ivars().attributes.as_ref().and_then(|a| a.objectForKey(key));
            given.or_else(|| object_for_key(&self.ivars().spec, key))
        }

        #[unsafe(method_id(fontAttributes))]
        fn font_attributes(&self) -> Retained<NSDictionary<NSString, AnyObject>> {
            self.attributes()
        }

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut Self {
            // Immutable: a copy is the same descriptor.
            Retained::into_raw(self.retain())
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other
                .and_then(|o| o.downcast_ref::<NSFontDescriptor>())
                .is_some_and(|o| descriptor_imp(o).ivars().spec == self.ivars().spec)
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            let spec = &self.ivars().spec;
            (spec.size.to_bits() as usize).rotate_left(7) ^ spec.weight.to_bits() as usize ^ usize::from(spec.italic)
        }
    }

    unsafe impl NSObjectProtocol for NSFontDescriptorImpl {}
);

fn with_design(spec: &FontSpec, design: &NSString) -> Option<Retained<NSFontDescriptorImpl>> {
    let Family::System(_) = spec.family else { return None };
    // SAFETY: the constants are this crate's own, always valid.
    let design = unsafe {
        if design == NSFontDescriptorSystemDesignMonospaced {
            Design::Monospaced
        } else if design == NSFontDescriptorSystemDesignSerif {
            Design::Serif
        } else if design == NSFontDescriptorSystemDesignRounded {
            Design::Rounded
        } else {
            Design::Default
        }
    };
    Some(new_descriptor(FontSpec { family: Family::System(design), ..spec.clone() }))
}

pub(crate) fn object_for_key(spec: &FontSpec, key: &NSString) -> Option<Retained<AnyObject>> {
    let face = fonts::resolve(spec);
    // SAFETY: the constants are this crate's own, always valid.
    let value = unsafe {
        if key == NSFontFamilyAttribute {
            &face.family_name
        } else if key == NSFontNameAttribute {
            &face.postscript_name
        } else if key == NSFontVisibleNameAttribute {
            &face.full_name
        } else {
            return None;
        }
    };
    Some(Retained::into_super(Retained::into_super(NSString::from_str(value))))
}

impl NSFontDescriptorImpl {
    /// The attributes it was made with, or those of what it describes.
    fn attributes(&self) -> Retained<NSDictionary<NSString, AnyObject>> {
        match &self.ivars().attributes {
            Some(given) => given.clone(),
            None => attributes_of(&self.ivars().spec),
        }
    }
}

/// `base` with `added`'s entries put in.
fn merged(
    base: &NSDictionary<NSString, AnyObject>,
    added: &NSDictionary<NSString, AnyObject>,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let out: Retained<objc2_foundation::NSMutableDictionary<NSString, AnyObject>> = base.mutableCopy();
    out.addEntriesFromDictionary(added);
    Retained::into_super(out)
}

/// What a descriptor of a font made from `spec` reports as its
/// attributes, as macOS reports a font's: its PostScript name, its size
/// (when it has one) and the feature settings it was made with.
pub(crate) fn attributes_of(spec: &FontSpec) -> Retained<NSDictionary<NSString, AnyObject>> {
    let face = fonts::resolve(spec);
    let name = match &spec.family {
        Family::Named(name) if spec.missing => any(NSString::from_str(name)),
        _ => any(NSString::from_str(&face.postscript_name)),
    };
    // SAFETY: the constants are this crate's own, always valid.
    let (name_key, size_key, features_key) =
        unsafe { (NSFontNameAttribute, NSFontSizeAttribute, NSFontFeatureSettingsAttribute) };
    let mut keys: Vec<&NSString> = vec![name_key];
    let mut values: Vec<Retained<AnyObject>> = vec![name];
    if spec.size > 0.0 {
        keys.push(size_key);
        values.push(any(objc2_foundation::NSNumber::new_f64(spec.size)));
    }
    if let Some(settings) = settings_of(spec) {
        keys.push(features_key);
        values.push(settings);
    }
    if let Some(variations) = spec.variations.as_ref().filter(|v| !v.is_empty()) {
        let tags: Vec<Retained<objc2_foundation::NSNumber>> =
            variations.iter().map(|(t, _)| objc2_foundation::NSNumber::new_u32(u32::from_be_bytes(*t))).collect();
        let vals: Vec<Retained<AnyObject>> =
            variations.iter().map(|(_, v)| any(objc2_foundation::NSNumber::new_f64(f64::from(*v)))).collect();
        let tag_refs: Vec<&objc2_foundation::NSNumber> = tags.iter().map(|t| &**t).collect();
        let val_refs: Vec<&AnyObject> = vals.iter().map(|v| &**v).collect();
        // SAFETY: the constant is this crate's own.
        keys.push(unsafe { objc2_app_kit::NSFontVariationAttribute });
        values.push(any(NSDictionary::<objc2_foundation::NSNumber, AnyObject>::from_slices(&tag_refs, &val_refs)));
    }
    let refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
    NSDictionary::from_slices(&keys, &refs)
}

/// A spec's feature settings as `NSFontFeatureSettingsAttribute` gives
/// them: by feature type and selector where the font feature registry
/// has the feature, by OpenType tag and value where it doesn't.
pub(crate) fn settings_of(spec: &FontSpec) -> Option<Retained<AnyObject>> {
    let features = spec.features.as_ref().filter(|f| !f.is_empty())?;
    // SAFETY: the constants are this crate's own, always valid.
    let (type_key, selector_key) = unsafe { (NSFontFeatureTypeIdentifierKey, NSFontFeatureSelectorIdentifierKey) };
    let items: Vec<Retained<NSDictionary<NSString, AnyObject>>> = features
        .iter()
        .map(|&(tag, value)| match registry_setting(tag, value) {
            Some((kind, selector)) => {
                let (k, s) = (objc2_foundation::NSNumber::new_i64(kind), objc2_foundation::NSNumber::new_i64(selector));
                NSDictionary::from_slices(&[type_key, selector_key], &[&*k as &AnyObject, &*s as &AnyObject])
            }
            None => {
                let t = NSString::from_str(&String::from_utf8_lossy(&tag));
                let v = objc2_foundation::NSNumber::new_i64(i64::from(value));
                let keys = <[&NSString; 2]>::from(opentype_keys());
                NSDictionary::from_slices(&keys, &[&*t as &AnyObject, &*v as &AnyObject])
            }
        })
        .collect();
    Some(any(objc2_foundation::NSArray::from_retained_slice(&items)))
}

/// The font feature registry's type and selector for an OpenType feature
/// turned on or off, if it has one.
pub(crate) fn registry_setting(tag: [u8; 4], value: u16) -> Option<(i64, i64)> {
    (0..40i64)
        .flat_map(|kind| (0..48i64).map(move |selector| (kind, selector)))
        .find(|&(kind, selector)| registry_feature(kind, selector) == Some((tag, value)))
}

fn descriptor_imp(descriptor: &NSFontDescriptor) -> &NSFontDescriptorImpl {
    // SAFETY: every NSFontDescriptor is an NSFontDescriptorImpl.
    unsafe { &*(descriptor as *const NSFontDescriptor).cast::<NSFontDescriptorImpl>() }
}

/// What `descriptor` describes.
pub(crate) fn descriptor_spec(descriptor: &NSFontDescriptor) -> FontSpec {
    descriptor_imp(descriptor).ivars().spec.clone()
}

fn new_descriptor(spec: FontSpec) -> Retained<NSFontDescriptorImpl> {
    new_descriptor_with(spec, None)
}

fn new_descriptor_with(
    spec: FontSpec,
    attributes: Option<Retained<NSDictionary<NSString, AnyObject>>>,
) -> Retained<NSFontDescriptorImpl> {
    crate::load_shell::<objc2_app_kit::NSFontDescriptor>();
    let this = NSFontDescriptorImpl::alloc().set_ivars(DescriptorIvars { spec, attributes });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A descriptor for `spec`, reporting `attributes` if given (see
/// [`DescriptorIvars`]).
pub(crate) fn make_descriptor(
    spec: FontSpec,
    attributes: Option<Retained<NSDictionary<NSString, AnyObject>>>,
) -> Retained<NSFontDescriptor> {
    load::<NSFontDescriptor>();
    // SAFETY: NSFontDescriptorImpl is NSFontDescriptor's implementation.
    unsafe { Retained::cast_unchecked(new_descriptor_with(spec, attributes)) }
}

fn descriptor(spec: FontSpec) -> Retained<NSFontDescriptor> {
    load::<NSFontDescriptor>();
    // SAFETY: NSFontDescriptorImpl is NSFontDescriptor's implementation.
    unsafe { Retained::cast_unchecked(new_descriptor(spec)) }
}

/// What a descriptor naming `name` asks for: a font the system has, or a
/// missing one that no font will be made from.
pub(crate) fn spec_of_name(name: &str, size: f64) -> FontSpec {
    fonts::spec_named(name, size).unwrap_or_else(|| FontSpec {
        family: Family::Named(name.into()),
        missing: true,
        ..FontSpec::system(Design::Default, size)
    })
}

pub(crate) fn traits_of(spec: &FontSpec) -> NSFontDescriptorSymbolicTraits {
    let mut traits = NSFontDescriptorSymbolicTraits::empty();
    if spec.italic {
        traits |= NSFontDescriptorSymbolicTraits::TraitItalic;
    }
    if spec.weight >= 600.0 {
        traits |= NSFontDescriptorSymbolicTraits::TraitBold;
    }
    if spec.stretch < 1.0 {
        traits |= NSFontDescriptorSymbolicTraits::TraitCondensed;
    } else if spec.stretch > 1.0 {
        traits |= NSFontDescriptorSymbolicTraits::TraitExpanded;
    }
    if spec.family == Family::System(Design::Monospaced) || fonts::resolve(spec).fixed_pitch {
        traits |= NSFontDescriptorSymbolicTraits::TraitMonoSpace;
    }
    traits
}

/// A number from an attribute dictionary: an `NSNumber`, or anything else
/// that answers `doubleValue`.
pub(crate) fn number(value: &AnyObject) -> Option<f64> {
    let sel: Sel = sel!(doubleValue);
    // SAFETY: respondsToSelector: takes a selector and returns BOOL, and a
    // doubleValue that exists returns a double.
    unsafe {
        let responds: bool = msg_send![value, respondsToSelector: sel];
        responds.then(|| msg_send![value, doubleValue])
    }
}

/// A descriptor for `attributes`, as `fontDescriptorWithFontAttributes:`
/// makes one.
pub(crate) fn spec_of_attributes(attributes: Option<&NSDictionary<NSString, AnyObject>>) -> FontSpec {
    let mut spec = FontSpec::system(Design::Default, 0.0);
    if let Some(attributes) = attributes {
        apply_attributes(&mut spec, attributes);
    }
    spec
}

/// Apply descriptor attributes to `spec`: the name or family, the size,
/// the traits (symbolic traits, weight and width) and feature settings.
pub(crate) fn apply_attributes(spec: &mut FontSpec, attributes: &NSDictionary<NSString, AnyObject>) {
    // SAFETY: the constants are this crate's own, always valid.
    let (name_key, family_key, size_key, traits_key, features_key, variation_key) = unsafe {
        (
            NSFontNameAttribute,
            NSFontFamilyAttribute,
            NSFontSizeAttribute,
            NSFontTraitsAttribute,
            NSFontFeatureSettingsAttribute,
            objc2_app_kit::NSFontVariationAttribute,
        )
    };
    let string = |key: &NSString| attributes.objectForKey(key).and_then(|o| o.downcast::<NSString>().ok());
    if let Some(size) = attributes.objectForKey(size_key).and_then(|v| number(&v)) {
        spec.size = size;
    }
    if let Some(name) = string(name_key) {
        let named = spec_of_name(&name.to_string(), spec.size);
        (spec.family, spec.weight, spec.italic, spec.stretch, spec.missing) =
            (named.family, named.weight, named.italic, named.stretch, named.missing);
    } else if let Some(family) = string(family_key) {
        let named = spec_of_name(&family.to_string(), 0.0);
        (spec.family, spec.missing) = (named.family, named.missing);
    }
    if let Some(traits) = attributes.objectForKey(traits_key).and_then(|t| t.downcast::<NSDictionary>().ok()) {
        // SAFETY: as above.
        let (symbolic_key, weight_key, width_key) =
            unsafe { (NSFontSymbolicTrait, NSFontWeightTrait, NSFontWidthTrait) };
        // SAFETY: the generic types are only a view; looking a string key up
        // is sound whatever the dictionary holds.
        let traits: Retained<NSDictionary<NSString, AnyObject>> = unsafe { Retained::cast_unchecked(traits) };
        if let Some(symbolic) = traits.objectForKey(symbolic_key).and_then(|v| number(&v)) {
            let symbolic = NSFontDescriptorSymbolicTraits(symbolic as u32);
            if symbolic.contains(NSFontDescriptorSymbolicTraits::TraitBold) {
                spec.weight = 700.0;
            }
            spec.italic |= symbolic.contains(NSFontDescriptorSymbolicTraits::TraitItalic);
            if symbolic.contains(NSFontDescriptorSymbolicTraits::TraitMonoSpace) {
                spec.family = Family::System(Design::Monospaced);
            }
        }
        if let Some(weight) = traits.objectForKey(weight_key).and_then(|v| number(&v)) {
            spec.weight = fonts::css_weight(weight);
        }
        if let Some(width) = traits.objectForKey(width_key).and_then(|v| number(&v)) {
            spec.stretch = stretch_of_width(width);
        }
    }
    if let Some(variation) = attributes.objectForKey(variation_key).and_then(|v| v.downcast::<NSDictionary>().ok()) {
        // Axes by tag (as a number) to values, over what the spec had.
        let mut values: Vec<([u8; 4], f32)> = spec.variations.iter().flat_map(|v| v.iter().copied()).collect();
        // SAFETY: the generic types are only a view; the keys are read as
        // objects.
        let variation: Retained<NSDictionary<AnyObject, AnyObject>> = unsafe { Retained::cast_unchecked(variation) };
        for key in variation.allKeys() {
            let (Some(tag), Some(value)) = (number(&key), variation.objectForKey(&key).and_then(|v| number(&v))) else {
                continue;
            };
            let tag = (tag as u32).to_be_bytes();
            values.retain(|v| v.0 != tag);
            values.push((tag, value as f32));
        }
        spec.variations = (!values.is_empty()).then(|| values.into());
    }
    if let Some(settings) = attributes.objectForKey(features_key) {
        let mut features: Vec<([u8; 4], u16)> = spec.features.iter().flat_map(|f| f.iter().copied()).collect();
        for (tag, value) in feature_settings(&settings) {
            features.retain(|f| f.0 != tag);
            features.push((tag, value));
        }
        spec.features = (!features.is_empty()).then(|| features.into());
    }
}

/// The items of an array: anything that answers `count` and
/// `objectAtIndex:`, as `NSArray` does.
pub(crate) fn array_items(array: &AnyObject) -> Vec<Retained<AnyObject>> {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL; an
    // object with objectAtIndex: is an array, whose count is an NSUInteger
    // and whose items are objects.
    unsafe {
        let responds: bool = msg_send![array, respondsToSelector: sel!(objectAtIndex:)];
        if !responds {
            return Vec::new();
        }
        let count: usize = msg_send![array, count];
        (0..count).map(|i| msg_send![array, objectAtIndex: i]).collect()
    }
}

/// OpenType features from an `NSFontFeatureSettingsAttribute` array, whose
/// items give a feature by type and selector, as Apple's font feature
/// registry numbers them, or by OpenType tag and value.
fn feature_settings(settings: &AnyObject) -> Vec<([u8; 4], u16)> {
    // SAFETY: the constants are this crate's own, always valid.
    let (type_key, selector_key) = unsafe { (NSFontFeatureTypeIdentifierKey, NSFontFeatureSelectorIdentifierKey) };
    let mut out = Vec::new();
    for item in array_items(settings) {
        let Ok(setting) = item.downcast::<NSDictionary>() else { continue };
        // SAFETY: as in `apply_attributes`.
        let setting: Retained<NSDictionary<NSString, AnyObject>> = unsafe { Retained::cast_unchecked(setting) };
        let value = |key: &NSString| setting.objectForKey(key).and_then(|v| number(&v));
        if let (Some(kind), Some(selector)) = (value(type_key), value(selector_key)) {
            out.extend(registry_feature(kind as i64, selector as i64));
        } else if let Some(tag) = setting.objectForKey(opentype_keys().0).and_then(|t| t.downcast::<NSString>().ok())
            && let Ok(tag) = <[u8; 4]>::try_from(tag.to_string().as_bytes())
        {
            let on = value(opentype_keys().1).unwrap_or(1.0);
            out.push((tag, on.clamp(0.0, f64::from(u16::MAX)) as u16));
        }
    }
    out
}

/// The OpenType feature a font feature registry type and selector turn on
/// or off (CoreText's `SFNTLayoutTypes` numbers, which
/// `NSFontFeatureSettingsAttribute` and `kCTFontFeatureSettingsAttribute`
/// take).
pub(crate) fn registry_feature(kind: i64, selector: i64) -> Option<([u8; 4], u16)> {
    let on_off =
        |tag: &[u8; 4], on: i64| (selector == on || selector == on + 1).then(|| (*tag, u16::from(selector == on)));
    let pick =
        |tags: &[[u8; 4]], from: i64| tags.get(usize::try_from(selector.checked_sub(from)?).ok()?).map(|t| (*t, 1));
    match kind {
        // Ligatures: required, common, rare, contextual, historical.
        1 => match selector {
            0 | 1 => on_off(b"rlig", 0),
            2 | 3 => on_off(b"liga", 2),
            4 | 5 => on_off(b"dlig", 4),
            18 | 19 => on_off(b"clig", 18),
            20 | 21 => on_off(b"hlig", 20),
            _ => None,
        },
        // Vertical forms.
        4 => on_off(b"vert", 0),
        // Number spacing: monospaced, proportional.
        6 => pick(&[*b"tnum", *b"pnum"], 0),
        // Vertical position: superiors, inferiors, ordinals, scientific
        // inferiors.
        10 => pick(&[*b"sups", *b"subs", *b"ordn", *b"sinf"], 1),
        // Fractions: none, vertical, diagonal.
        11 => [(*b"frac", 0), (*b"afrc", 1), (*b"frac", 1)].get(usize::try_from(selector).ok()?).copied(),
        // Typographic extras: slashed zero.
        14 => on_off(b"zero", 4),
        // Number case: lower case (old style), upper case (lining).
        21 => pick(&[*b"onum", *b"lnum"], 0),
        // Text spacing: proportional, full, half, third, quarter and
        // alternate widths; then kerning on (7) and off (8).
        22 => match selector {
            0..=6 => pick(&[*b"pwid", *b"fwid", *b"hwid", *b"twid", *b"qwid", *b"palt", *b"halt"], 0),
            7 => Some((*b"kern", 1)),
            8 => Some((*b"kern", 0)),
            _ => None,
        },
        // Case-sensitive layout and spacing.
        33 => match selector {
            0 | 1 => on_off(b"case", 0),
            2 | 3 => on_off(b"cpsp", 2),
            _ => None,
        },
        // Stylistic sets, two selectors (on, off) each from 2.
        35 if (2..42).contains(&selector) => {
            let n = selector / 2;
            Some(([b's', b's', b'0' + (n / 10) as u8, b'0' + (n % 10) as u8], u16::from(selector % 2 == 0)))
        }
        // Contextual alternates, swashes, contextual swashes.
        36 => match selector {
            0 | 1 => on_off(b"calt", 0),
            2 | 3 => on_off(b"swsh", 2),
            4 | 5 => on_off(b"cswh", 4),
            _ => None,
        },
        // Lower case: small and petite capitals.
        37 => pick(&[*b"smcp", *b"pcap"], 1),
        // Upper case: small and petite capitals.
        38 => pick(&[*b"c2sc", *b"c2pc"], 1),
        _ => None,
    }
}

// For the font manager (`font_manager`).

/// What `font` was made from.
pub(crate) fn spec_of(font: &NSFont) -> FontSpec {
    imp(font).ivars().spec.clone()
}

/// The face `font` resolved to.
pub(crate) fn face_of(font: &NSFont) -> Arc<Face> {
    imp(font).ivars().face.clone()
}

/// What `font` was made from and the face it resolved to, borrowed.
pub(crate) fn parts(font: &NSFont) -> (&FontSpec, &Arc<Face>) {
    let ivars = imp(font).ivars();
    (&ivars.spec, &ivars.face)
}

/// The font `spec` describes, whose size 0 is 12 points, as an `NSFont`.
pub(crate) fn make_font(spec: FontSpec) -> Retained<NSFont> {
    let made = font(spec);
    load::<NSFont>();
    // SAFETY: NSFontImpl is NSFont's implementation.
    unsafe { Retained::cast_unchecked(made) }
}

/// A font made from `spec`, with `like`'s default size.
pub(crate) fn font_like(like: &NSFont, spec: FontSpec) -> Retained<NSFont> {
    let made = font_with_zero(spec, imp(like).ivars().zero);
    load::<NSFont>();
    // SAFETY: NSFontImpl is NSFont's implementation.
    unsafe { Retained::cast_unchecked(made) }
}

/// The font `name` names, at `size`, if the system has it (as
/// `fontWithName:size:` finds it).
pub(crate) fn named(name: &str, size: f64) -> Option<FontSpec> {
    fonts::spec_named(name, size)
}

/// The keys of a feature setting by OpenType tag and value
/// (`kCTFontOpenTypeFeatureTag`, `kCTFontOpenTypeFeatureValue`).
fn opentype_keys() -> (&'static NSString, &'static NSString) {
    // SAFETY: the constants are this crate's own.
    unsafe {
        (
            crate::coretext::ns_string(objc2_core_text::kCTFontOpenTypeFeatureTag),
            crate::coretext::ns_string(objc2_core_text::kCTFontOpenTypeFeatureValue),
        )
    }
}

#[cfg(test)]
mod tests {
    use objc2::DefinedClass;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObjectProtocol};
    use objc2_app_kit::{NSFont, NSFontDescriptor, NSFontFeatureSettingsAttribute};
    use objc2_foundation::{NSDictionary, NSString};

    use super::imp;
    use crate::test_objects::{array, number};

    fn registry(kind: f64, selector: f64) -> Retained<AnyObject> {
        // SAFETY: the keys are constant strings.
        let keys = unsafe { [super::NSFontFeatureTypeIdentifierKey, super::NSFontFeatureSelectorIdentifierKey] };
        let (kind, selector) = (number(kind), number(selector));
        let setting = NSDictionary::from_slices(&keys, &[&*kind as &AnyObject, &*selector as &AnyObject]);
        Retained::into_super(Retained::into_super(setting))
    }

    fn opentype(tag: &str, value: f64) -> Retained<AnyObject> {
        let (tag, value) = (NSString::from_str(tag), number(value));
        let keys = <[&NSString; 2]>::from(super::opentype_keys());
        let setting = NSDictionary::from_slices(&keys, &[&*tag as &AnyObject, &*value as &AnyObject]);
        Retained::into_super(Retained::into_super(setting))
    }

    #[test]
    fn feature_settings_turn_features_on_and_off() {
        let settings = array(vec![registry(1.0, 3.0), registry(36.0, 1.0), registry(6.0, 0.0), opentype("ss02", 1.0)]);
        // SAFETY: the key is a constant string.
        let key = unsafe { NSFontFeatureSettingsAttribute };
        let attributes = NSDictionary::from_slices(&[key], &[&*settings as &AnyObject]);
        let base = NSFont::systemFontOfSize(13.0);
        // SAFETY: the attributes hold a feature settings array.
        let descriptor = unsafe { base.fontDescriptor().fontDescriptorByAddingAttributes(&attributes) };
        let font = NSFont::fontWithDescriptor_size(&descriptor, 0.0).unwrap();
        assert_eq!(font.pointSize(), 13.0);
        let features = imp(&font).ivars().spec.features.clone().expect("features");
        assert_eq!(*features, [(*b"liga", 0), (*b"calt", 0), (*b"tnum", 1), (*b"ss02", 1)]);
        assert!(!font.isEqual(Some(&base)), "a font with other features is another font");
        // SAFETY: as above.
        let same = unsafe { NSFontDescriptor::fontDescriptorWithFontAttributes(Some(&attributes)) };
        assert_eq!(imp(&NSFont::fontWithDescriptor_size(&same, 13.0).unwrap()).ivars().spec.features, Some(features));
    }
}
