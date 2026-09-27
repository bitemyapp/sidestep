//! `NSFontManager`: the shared font manager's conversions between fonts of
//! a family by trait, weight, size and face, and the families the system
//! has. There is no font panel or font menu (`fontPanel:` and
//! `fontMenu:` give nil); the action (`changeFont:`), target, delegate and
//! selected font are kept for programs that read them back.
//!
//! As measured on macOS: `+sharedFontManager` is one object, and
//! `alloc`/`init` gives that object too; a conversion that can't be made
//! (a trait the family has no face for, or one the font has already)
//! gives back the very font it was given. Weights are AppKit's 0-15 scale
//! (5 regular, 6 medium, 8 semibold, 9 bold), which maps to CSS weights.

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSFont, NSFontTraitMask};
use objc2_foundation::{NSArray, NSString};

use crate::font;
use crate::text::fonts::{Design, Family, FontSpec};

sidestep_runtime::static_class!(pub NSFONTMANAGER, NSFONTMANAGER_META = "NSFontManager", || {
    let _ = NSFontManagerImpl::class();
});

pub(crate) struct ManagerIvars {
    action: Cell<Option<Sel>>,
    target: RefCell<Weak<AnyObject>>,
    delegate: RefCell<Weak<AnyObject>>,
    enabled: Cell<bool>,
    selected: RefCell<Option<Retained<NSFont>>>,
    multiple: Cell<bool>,
}

thread_local! {
    /// The shared manager, made on first use and kept for the program's
    /// life.
    static SHARED: RefCell<Option<Retained<NSFontManagerImpl>>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSFontManager"]
    #[ivars = ManagerIvars]
    pub(crate) struct NSFontManagerImpl;

    impl NSFontManagerImpl {
        #[unsafe(method_id(sharedFontManager))]
        fn shared_font_manager() -> Retained<Self> {
            shared()
        }

        /// The shared manager, whoever asks.
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init(this)
        }

        #[unsafe(method(traitsOfFont:))]
        fn traits_of_font(&self, font: &NSFont) -> NSFontTraitMask {
            traits(&font::spec_of(font), font)
        }

        #[unsafe(method(weightOfFont:))]
        fn weight_of_font(&self, font: &NSFont) -> isize {
            appkit_weight(font::face_of(font).weight.max(font::spec_of(font).weight))
        }

        #[unsafe(method_id(convertFont:toHaveTrait:))]
        fn convert_font_to_have_trait(&self, font: &NSFont, trait_: NSFontTraitMask) -> Retained<NSFont> {
            convert(font, trait_, true)
        }

        #[unsafe(method_id(convertFont:toNotHaveTrait:))]
        fn convert_font_to_not_have_trait(&self, font: &NSFont, trait_: NSFontTraitMask) -> Retained<NSFont> {
            convert(font, trait_, false)
        }

        #[unsafe(method_id(convertWeight:ofFont:))]
        fn convert_weight(&self, up: bool, font: &NSFont) -> Retained<NSFont> {
            step_weight(font, up)
        }

        #[unsafe(method_id(convertFont:toSize:))]
        fn convert_font_to_size(&self, font: &NSFont, size: f64) -> Retained<NSFont> {
            let mut spec = font::spec_of(font);
            spec.size = size;
            font::font_like(font, spec)
        }

        #[unsafe(method_id(convertFont:toFamily:))]
        fn convert_font_to_family(&self, font: &NSFont, family: &NSString) -> Retained<NSFont> {
            to_family(font, family)
        }

        #[unsafe(method_id(convertFont:toFace:))]
        fn convert_font_to_face(&self, font: &NSFont, face: &NSString) -> Option<Retained<NSFont>> {
            to_face(font, face)
        }

        #[unsafe(method_id(convertFont:))]
        fn convert_font(&self, font: &NSFont) -> Retained<NSFont> {
            font.retain()
        }

        /// A font of the family with the traits, AppKit weight (or bold
        /// from the traits) and size; nil for a family the system lacks.
        #[unsafe(method_id(fontWithFamily:traits:weight:size:))]
        fn font_with_family(
            &self,
            family: &NSString,
            traits: NSFontTraitMask,
            weight: isize,
            size: f64,
        ) -> Option<Retained<NSFont>> {
            with_family(family, traits, weight, size)
        }

        #[unsafe(method_id(availableFontFamilies))]
        fn available_font_families(&self) -> Retained<NSArray<NSString>> {
            let mut names: Vec<String> =
                crate::text::with_ctx(|ctx| ctx.fcx.collection.family_names().map(String::from).collect());
            names.sort_by_key(|n| n.to_lowercase());
            names.dedup();
            let strings: Vec<Retained<NSString>> = names.iter().map(|n| NSString::from_str(n)).collect();
            NSArray::from_retained_slice(&strings)
        }

        #[unsafe(method(fontNamed:hasTraits:))]
        fn font_named_has_traits(&self, name: &NSString, wanted: NSFontTraitMask) -> bool {
            match font::named(&name.to_string(), 12.0) {
                Some(spec) => {
                    let made = font::font_like(&NSFont::systemFontOfSize(0.0), spec.clone());
                    traits(&spec, &made).contains(wanted)
                }
                None => false,
            }
        }

        #[unsafe(method(action))]
        fn action(&self) -> Option<Sel> {
            self.ivars().action.get()
        }

        #[unsafe(method(setAction:))]
        fn set_action(&self, action: Option<Sel>) {
            self.ivars().action.set(action);
        }

        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> {
            self.ivars().target.borrow().load()
        }

        #[unsafe(method(setTarget:))]
        fn set_target(&self, target: Option<&AnyObject>) {
            self.ivars().target.replace(target.map_or_else(Weak::default, Weak::new));
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().load()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            self.ivars().delegate.replace(delegate.map_or_else(Weak::default, Weak::new));
        }

        #[unsafe(method(isEnabled))]
        fn is_enabled(&self) -> bool {
            self.ivars().enabled.get()
        }

        #[unsafe(method(setEnabled:))]
        fn set_enabled(&self, enabled: bool) {
            self.ivars().enabled.set(enabled);
        }

        #[unsafe(method_id(selectedFont))]
        fn selected_font(&self) -> Option<Retained<NSFont>> {
            self.ivars().selected.borrow().clone()
        }

        #[unsafe(method(setSelectedFont:isMultiple:))]
        fn set_selected_font(&self, font: &NSFont, multiple: bool) {
            let old = self.ivars().selected.replace(Some(font.retain()));
            drop(old);
            self.ivars().multiple.set(multiple);
        }

        #[unsafe(method(isMultiple))]
        fn is_multiple(&self) -> bool {
            self.ivars().multiple.get()
        }

        /// No font panel.
        #[unsafe(method_id(fontPanel:))]
        fn font_panel(&self, _create: bool) -> Option<Retained<AnyObject>> {
            None
        }

        /// No font menu.
        #[unsafe(method_id(fontMenu:))]
        fn font_menu(&self, _create: bool) -> Option<Retained<AnyObject>> {
            None
        }
    }

    unsafe impl NSObjectProtocol for NSFontManagerImpl {}
);

fn init(this: Allocated<NSFontManagerImpl>) -> Retained<NSFontManagerImpl> {
    if let Some(shared) = SHARED.with_borrow(Clone::clone) {
        drop(this);
        return shared;
    }
    let this = this.set_ivars(ManagerIvars {
        action: Cell::new(Some(sel!(changeFont:))),
        target: RefCell::default(),
        delegate: RefCell::default(),
        enabled: Cell::new(true),
        selected: RefCell::new(None),
        multiple: Cell::new(false),
    });
    // SAFETY: NSObject's designated initializer.
    let made: Retained<NSFontManagerImpl> = unsafe { msg_send![super(this), init] };
    SHARED.with_borrow_mut(|s| s.get_or_insert(made).clone())
}

fn to_family(font: &NSFont, family: &NSString) -> Retained<NSFont> {
    let Some(named) = font::named(&family.to_string(), 0.0) else { return font.retain() };
    let mut spec = font::spec_of(font);
    spec.family = named.family;
    spec.missing = false;
    font::font_like(font, spec)
}

fn to_face(font: &NSFont, face: &NSString) -> Option<Retained<NSFont>> {
    let mut spec = font::named(&face.to_string(), 0.0)?;
    spec.size = font::spec_of(font).size;
    Some(font::font_like(font, spec))
}

fn with_family(family: &NSString, traits: NSFontTraitMask, weight: isize, size: f64) -> Option<Retained<NSFont>> {
    let named = font::named(&family.to_string(), size)?;
    let mut spec = FontSpec { family: named.family, size, ..FontSpec::system(Design::Default, size) };
    spec.weight = if traits.contains(NSFontTraitMask::BoldFontMask) { 700.0 } else { css_weight(weight) };
    spec.italic = traits.contains(NSFontTraitMask::ItalicFontMask);
    if traits.contains(NSFontTraitMask::CondensedFontMask) {
        spec.stretch = 0.75;
    } else if traits.contains(NSFontTraitMask::ExpandedFontMask) {
        spec.stretch = 1.25;
    }
    let base = NSFont::systemFontOfSize(0.0);
    Some(font::font_like(&base, spec))
}

fn shared() -> Retained<NSFontManagerImpl> {
    if let Some(shared) = SHARED.with_borrow(Clone::clone) {
        return shared;
    }
    let mtm = MainThreadMarker::new().expect("sidestep: the font manager belongs to the main thread");
    // SAFETY: the class is loaded (a message to it got here); init makes
    // the shared manager.
    unsafe { msg_send![NSFontManagerImpl::alloc(mtm), init] }
}

/// A font's traits as NSFontManager counts them.
fn traits(spec: &FontSpec, font: &NSFont) -> NSFontTraitMask {
    let face = font::face_of(font);
    let mut t = NSFontTraitMask::empty();
    if spec.italic || face.italic {
        t |= NSFontTraitMask::ItalicFontMask;
    }
    if spec.weight >= 600.0 {
        t |= NSFontTraitMask::BoldFontMask;
    }
    if face.stretch < 1.0 {
        t |= NSFontTraitMask::CondensedFontMask;
    } else if face.stretch > 1.0 {
        t |= NSFontTraitMask::ExpandedFontMask;
    }
    if face.fixed_pitch || spec.family == Family::System(Design::Monospaced) {
        t |= NSFontTraitMask::FixedPitchFontMask;
    }
    t
}

/// AppKit's weight (0 to 15) for a CSS weight.
fn appkit_weight(css: f32) -> isize {
    match css {
        w if w < 150.0 => 2,
        w if w < 250.0 => 3,
        w if w < 350.0 => 4,
        w if w < 450.0 => 5,
        w if w < 550.0 => 6,
        w if w < 650.0 => 8,
        w if w < 750.0 => 9,
        w if w < 850.0 => 10,
        _ => 11,
    }
}

/// The CSS weight for AppKit's `weight`.
fn css_weight(weight: isize) -> f32 {
    match weight {
        ..=2 => 100.0,
        3 => 200.0,
        4 => 300.0,
        5 => 400.0,
        6 | 7 => 500.0,
        8 => 600.0,
        9 => 700.0,
        10 => 800.0,
        _ => 900.0,
    }
}

/// `font` with `trait_` added (or taken away), or `font` itself when that
/// changes nothing it has a face for.
fn convert(font: &NSFont, trait_: NSFontTraitMask, add: bool) -> Retained<NSFont> {
    let before = font::spec_of(font);
    let mut spec = before.clone();
    let bold = spec.weight >= 600.0;
    let (bold_mask, unbold_mask) = (NSFontTraitMask::BoldFontMask, NSFontTraitMask::UnboldFontMask);
    let (italic_mask, unitalic_mask) = (NSFontTraitMask::ItalicFontMask, NSFontTraitMask::UnitalicFontMask);
    let wants_bold = (add && trait_.contains(bold_mask)) || (!add && trait_.contains(unbold_mask));
    let wants_unbold = (add && trait_.contains(unbold_mask)) || (!add && trait_.contains(bold_mask));
    if wants_bold && !bold {
        spec.weight = 700.0;
    } else if wants_unbold && !wants_bold && bold {
        spec.weight = 400.0;
    }
    if (add && trait_.contains(italic_mask)) || (!add && trait_.contains(unitalic_mask)) {
        spec.italic = true;
    } else if (add && trait_.contains(unitalic_mask)) || (!add && trait_.contains(italic_mask)) {
        spec.italic = false;
    }
    let widths = NSFontTraitMask::CondensedFontMask | NSFontTraitMask::ExpandedFontMask;
    if trait_.intersects(widths) {
        spec.stretch = match (add, trait_.contains(NSFontTraitMask::CondensedFontMask)) {
            (true, true) => 0.75,
            (true, false) => 1.25,
            (false, _) => 1.0,
        };
    }
    if spec.key() == before.key() {
        return font.retain();
    }
    let made = font::font_like(font, spec);
    // A width the family has no face for leaves the font as it was.
    if trait_.intersects(widths) && font::face_of(&made).stretch == font::face_of(font).stretch {
        return font.retain();
    }
    made
}

/// The next heavier (or lighter) face of the font's family, or the font.
fn step_weight(font: &NSFont, up: bool) -> Retained<NSFont> {
    let spec = font::spec_of(font);
    let now = font::face_of(font).weight;
    let mut weight = spec.weight;
    loop {
        weight += if up { 100.0 } else { -100.0 };
        if !(100.0..=900.0).contains(&weight) {
            return font.retain();
        }
        let next = FontSpec { weight, ..spec.clone() };
        let made = font::font_like(font, next);
        let face = font::face_of(&made).weight;
        if (up && face > now) || (!up && face < now) {
            return made;
        }
    }
}
