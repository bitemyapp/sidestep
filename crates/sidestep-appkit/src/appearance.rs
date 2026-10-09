//! `NSAppearance`: the named appearances, which one drawing happens in,
//! and how views, windows and the application inherit them.
//!
//! Appearances are singletons, one per name, made on first use and never
//! freed. What a color resolves to depends on the *current drawing
//! appearance*: the innermost `performAsCurrentDrawingAppearance:` block,
//! else the view being drawn (the display pass sets it around each
//! `drawRect:`), else the application's effective appearance, which is
//! its own if it set one and otherwise the desktop's (light or dark, and
//! high contrast, from `settings`).
//!
//! A view's effective appearance is its own, else its superview's, else
//! its window's, else the application's. Views cache it, so drawing reads
//! it without walking up; a change anywhere (a view's, a window's, the
//! application's or the desktop's appearance, or a view moving under
//! another parent) walks the views under it, and each view whose effective
//! appearance changed updates its cache, gets
//! `viewDidChangeEffectiveAppearance` and is redrawn.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{NSAppearance, NSAppearanceName, NSView};
use objc2_foundation::{NSArray, NSString};

use crate::palette::Look;

/// The appearances there are, in the order of their names below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Id {
    Aqua,
    DarkAqua,
    VibrantLight,
    VibrantDark,
    ContrastAqua,
    ContrastDarkAqua,
    ContrastVibrantLight,
    ContrastVibrantDark,
}

const ALL: [Id; 8] = [
    Id::Aqua,
    Id::DarkAqua,
    Id::VibrantLight,
    Id::VibrantDark,
    Id::ContrastAqua,
    Id::ContrastDarkAqua,
    Id::ContrastVibrantLight,
    Id::ContrastVibrantDark,
];

// The names, with AppKit's values (conformance/tests/color_appearance.rs
// compares). High-contrast appearances answer `name` with their base
// appearance's name, as AppKit's do.
sidestep_foundation::constant_string!(NSAppearanceNameAqua = "NSAppearanceNameAqua");
sidestep_foundation::constant_string!(NSAppearanceNameDarkAqua = "NSAppearanceNameDarkAqua");
sidestep_foundation::constant_string!(NSAppearanceNameVibrantLight = "NSAppearanceNameVibrantLight");
sidestep_foundation::constant_string!(NSAppearanceNameVibrantDark = "NSAppearanceNameVibrantDark");
sidestep_foundation::constant_string!(NSAppearanceNameLightContent = "NSAppearanceNameLightContent");
sidestep_foundation::constant_string!(
    NSAppearanceNameAccessibilityHighContrastAqua = "NSAppearanceNameAccessibilityAqua"
);
sidestep_foundation::constant_string!(
    NSAppearanceNameAccessibilityHighContrastDarkAqua = "NSAppearanceNameAccessibilityDarkAqua"
);
sidestep_foundation::constant_string!(
    NSAppearanceNameAccessibilityHighContrastVibrantLight = "NSAppearanceNameAccessibilityVibrantLight"
);
sidestep_foundation::constant_string!(
    NSAppearanceNameAccessibilityHighContrastVibrantDark = "NSAppearanceNameAccessibilityVibrantDark"
);

impl Id {
    /// The name `appearanceNamed:` takes.
    fn full_name(self) -> &'static str {
        match self {
            Id::Aqua => "NSAppearanceNameAqua",
            Id::DarkAqua => "NSAppearanceNameDarkAqua",
            Id::VibrantLight => "NSAppearanceNameVibrantLight",
            Id::VibrantDark => "NSAppearanceNameVibrantDark",
            Id::ContrastAqua => "NSAppearanceNameAccessibilityAqua",
            Id::ContrastDarkAqua => "NSAppearanceNameAccessibilityDarkAqua",
            Id::ContrastVibrantLight => "NSAppearanceNameAccessibilityVibrantLight",
            Id::ContrastVibrantDark => "NSAppearanceNameAccessibilityVibrantDark",
        }
    }

    /// The appearance without high contrast.
    fn base(self) -> Id {
        match self {
            Id::ContrastAqua => Id::Aqua,
            Id::ContrastDarkAqua => Id::DarkAqua,
            Id::ContrastVibrantLight => Id::VibrantLight,
            Id::ContrastVibrantDark => Id::VibrantDark,
            id => id,
        }
    }

    fn from_name(name: &str) -> Option<Id> {
        if name == "NSAppearanceNameLightContent" {
            return Some(Id::Aqua);
        }
        ALL.into_iter().find(|id| id.full_name() == name)
    }

    pub fn look(self) -> Look {
        match self {
            Id::Aqua | Id::VibrantLight => Look::Light,
            Id::DarkAqua | Id::VibrantDark => Look::Dark,
            Id::ContrastAqua | Id::ContrastVibrantLight => Look::LightContrast,
            Id::ContrastDarkAqua | Id::ContrastVibrantDark => Look::DarkContrast,
        }
    }

    fn vibrant(self) -> bool {
        matches!(self.base(), Id::VibrantLight | Id::VibrantDark)
    }

    /// The appearance of a look, not vibrant.
    pub fn of_look(look: Look) -> Id {
        match look {
            Look::Light => Id::Aqua,
            Look::Dark => Id::DarkAqua,
            Look::LightContrast => Id::ContrastAqua,
            Look::DarkContrast => Id::ContrastDarkAqua,
        }
    }

    /// How well an appearance of name `candidate` suits this one: an exact
    /// match, then the same appearance without high contrast, then
    /// without vibrancy; `None` if it doesn't (light for dark).
    fn suitability(self, candidate: Id) -> Option<u8> {
        if candidate == self {
            return Some(0);
        }
        if candidate == self.base() {
            return Some(1);
        }
        let plain = |id: Id| match id.base() {
            Id::VibrantLight => Id::Aqua,
            Id::VibrantDark => Id::DarkAqua,
            id => id,
        };
        if plain(candidate) == plain(self) && !candidate.vibrant() {
            return Some(2 + u8::from(candidate != candidate.base()));
        }
        None
    }
}

pub(crate) struct AppearanceIvars {
    id: Id,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; appearances are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "NSAppearance"]
    #[ivars = AppearanceIvars]
    pub(crate) struct NSAppearanceImpl;

    impl NSAppearanceImpl {
        #[unsafe(method_id(appearanceNamed:))]
        fn appearance_named(name: &NSAppearanceName) -> Option<Retained<NSAppearance>> {
            Id::from_name(&name.to_string()).map(get)
        }

        #[unsafe(method_id(initWithAppearanceNamed:bundle:))]
        fn init_with_name(_this: Allocated<Self>, name: &NSAppearanceName, _bundle: Option<&NSObject>) -> Option<Retained<Self>> {
            // The shared instance, as the name is one of these.
            // SAFETY: NSAppearanceImpl is the class NSAppearance names.
            Id::from_name(&name.to_string()).map(|id| unsafe { Retained::cast_unchecked(get(id)) })
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Retained<NSAppearanceName> {
            NSString::from_str(self.ivars().id.base().full_name())
        }

        #[unsafe(method(allowsVibrancy))]
        fn allows_vibrancy(&self) -> bool {
            self.ivars().id.vibrant()
        }

        #[unsafe(method_id(bestMatchFromAppearancesWithNames:))]
        fn best_match(&self, names: &NSArray<NSAppearanceName>) -> Option<Retained<NSAppearanceName>> {
            let id = self.ivars().id;
            let mut best: Option<(u8, Retained<NSAppearanceName>)> = None;
            for name in names.iter() {
                let Some(candidate) = Id::from_name(&name.to_string()) else { continue };
                if let Some(score) = id.suitability(candidate)
                    && best.as_ref().is_none_or(|(b, _)| score < *b)
                {
                    best = Some((score, name));
                }
            }
            best.map(|(_, name)| name)
        }

        #[unsafe(method_id(currentAppearance))]
        fn current_appearance() -> Option<Retained<NSAppearance>> {
            CURRENT_APPEARANCE.with(|c| c.get()).map(get)
        }

        #[unsafe(method(setCurrentAppearance:))]
        fn set_current_appearance(appearance: Option<&NSAppearance>) {
            CURRENT_APPEARANCE.with(|c| c.set(appearance.map(id_of)));
        }

        #[unsafe(method_id(currentDrawingAppearance))]
        fn current_drawing_appearance() -> Retained<NSAppearance> {
            get(current())
        }

        #[unsafe(method(performAsCurrentDrawingAppearance:))]
        fn perform_as_current(&self, block: &DynBlock<dyn Fn() + '_>) {
            let _drawing = Drawing::push(self.ivars().id);
            block.call(());
        }
    }

    unsafe impl NSObjectProtocol for NSAppearanceImpl {}
);

/// The appearance singleton for `id`.
pub(crate) fn get(id: Id) -> Retained<NSAppearance> {
    static ALL_OF_THEM: OnceLock<[usize; 8]> = OnceLock::new();
    let all = ALL_OF_THEM.get_or_init(|| {
        crate::load_shell::<NSAppearance>();
        ALL.map(|id| {
            let this = NSAppearanceImpl::alloc().set_ivars(AppearanceIvars { id });
            // SAFETY: NSObject's designated initializer.
            let this: Retained<NSAppearanceImpl> = unsafe { msg_send![super(this), init] };
            // Kept for the life of the program.
            Retained::into_raw(this) as usize
        })
    });
    let ptr = all[id as usize] as *mut NSAppearance;
    // SAFETY: the singletons are never released, and retaining one gives
    // the caller its own reference.
    unsafe { Retained::retain(ptr) }.expect("an appearance")
}

/// Which appearance `a` is.
pub(crate) fn id_of(a: &NSAppearance) -> Id {
    // SAFETY: every NSAppearance is an NSAppearanceImpl.
    unsafe { &*(a as *const NSAppearance).cast::<NSAppearanceImpl>() }.ivars().id
}

thread_local! {
    /// `performAsCurrentDrawingAppearance:` blocks and views being drawn,
    /// innermost last.
    static DRAWING: RefCell<Vec<Id>> = const { RefCell::new(Vec::new()) };
    /// The top of `DRAWING`, for colors to read without a borrow.
    static TOP: Cell<Option<Id>> = const { Cell::new(None) };
    /// `+[NSAppearance setCurrentAppearance:]`, deprecated but honored.
    static CURRENT_APPEARANCE: Cell<Option<Id>> = const { Cell::new(None) };
}

/// `-[NSApplication setAppearance:]`, for every thread: 0 for none, else
/// one more than the appearance's index.
static APP: AtomicU8 = AtomicU8::new(0);

/// While alive, `id` is the current drawing appearance.
pub(crate) struct Drawing;

impl Drawing {
    pub fn push(id: Id) -> Drawing {
        DRAWING.with(|d| d.borrow_mut().push(id));
        TOP.with(|t| t.set(Some(id)));
        Drawing
    }
}

impl Drop for Drawing {
    fn drop(&mut self) {
        let top = DRAWING.with(|d| {
            let mut d = d.borrow_mut();
            d.pop();
            d.last().copied()
        });
        TOP.with(|t| t.set(top));
    }
}

/// The current drawing appearance.
pub(crate) fn current() -> Id {
    TOP.with(Cell::get).or_else(|| CURRENT_APPEARANCE.with(Cell::get)).unwrap_or_else(app_effective)
}

/// The look colors resolve in now.
pub(crate) fn current_look() -> Look {
    current().look()
}

/// The desktop's appearance (kept by `sidestep_engine::settings`).
pub(crate) fn system() -> Id {
    Id::of_look(sidestep_engine::settings::system_look())
}

pub(crate) fn app_appearance() -> Option<Id> {
    ALL.get(usize::from(APP.load(Ordering::Relaxed)).checked_sub(1)?).copied()
}

pub(crate) fn set_app_appearance(id: Option<Id>) {
    APP.store(id.map_or(0, |id| id as u8 + 1), Ordering::Relaxed);
}

pub(crate) fn app_effective() -> Id {
    app_appearance().unwrap_or_else(system)
}

// Views and windows.

/// What a view keeps about its appearance: its own, if it set one, and its
/// effective one as last worked out.
#[derive(Default)]
pub(crate) struct ViewAppearance {
    pub own: Cell<Option<Id>>,
    pub effective: Cell<Option<Id>>,
}

/// A view's effective appearance, from its cache or worked out (and
/// cached).
pub(crate) fn effective(view: &crate::views::NSViewImpl) -> Id {
    let slot = crate::views::appearance_slot(view);
    if let Some(id) = slot.effective.get() {
        return id;
    }
    let id = inherited(view);
    slot.effective.set(Some(id));
    id
}

/// What a view's effective appearance is by the rules, cache aside.
fn inherited(view: &crate::views::NSViewImpl) -> Id {
    if let Some(id) = crate::views::appearance_slot(view).own.get() {
        return id;
    }
    if let Some(sup) = crate::views::superview_of(view) {
        return effective(sup);
    }
    if let Some(window) = crate::views::window_of(view) {
        return window_effective(window);
    }
    app_effective()
}

pub(crate) fn window_effective(window: &crate::window::NSWindowImpl) -> Id {
    crate::window::appearance_slot(window).get().unwrap_or_else(app_effective)
}

/// Something changed that `view`'s subtree inherits: work its views'
/// effective appearances out again, telling those whose changed.
pub(crate) fn refresh(view: &NSView) {
    let mut changed = Vec::new();
    walk(crate::views::imp(view), &mut changed);
    // Tell views after the walk: an override may change appearances
    // itself, and no borrow is held while it runs.
    for v in changed {
        // SAFETY: viewDidChangeEffectiveAppearance takes nothing.
        let _: () = unsafe { msg_send![&*v, viewDidChangeEffectiveAppearance] };
        v.setNeedsDisplay(true);
    }
}

fn walk(view: &crate::views::NSViewImpl, changed: &mut Vec<Retained<NSView>>) {
    let slot = crate::views::appearance_slot(view);
    let old = slot.effective.take();
    let new = inherited(view);
    slot.effective.set(Some(new));
    if old.is_some_and(|old| old != new) {
        changed.push(crate::views::as_view(view).retain());
    }
    // Subviews that never worked theirs out have nothing to tell.
    for sub in crate::views::subviews(view) {
        walk(crate::views::imp(&sub), changed);
    }
}

/// The application's or the desktop's appearance changed: refresh every
/// window.
pub(crate) fn refresh_all() {
    for window in crate::app::windows_for_appearance() {
        if let Some(content) = window.contentView() {
            refresh(&content);
        }
        crate::window::imp(&window).damage_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_matches_prefer_exact_then_base_then_plain() {
        use Id::*;
        let best = |id: Id, names: &[Id]| {
            names.iter().filter_map(|&n| id.suitability(n).map(|s| (s, n))).min_by_key(|(s, _)| *s).map(|(_, n)| n)
        };
        assert_eq!(best(VibrantDark, &[Aqua, DarkAqua]), Some(DarkAqua));
        assert_eq!(best(VibrantLight, &[Aqua, DarkAqua]), Some(Aqua));
        assert_eq!(best(ContrastDarkAqua, &[Aqua, DarkAqua]), Some(DarkAqua));
        assert_eq!(best(ContrastDarkAqua, &[DarkAqua, ContrastDarkAqua]), Some(ContrastDarkAqua));
        assert_eq!(best(DarkAqua, &[Aqua]), None);
        assert_eq!(best(Aqua, &[VibrantLight]), None, "vibrancy isn't added");
    }

    #[test]
    fn drawing_appearances_nest() {
        assert_eq!(TOP.with(Cell::get), None);
        {
            let _outer = Drawing::push(Id::DarkAqua);
            assert_eq!(current(), Id::DarkAqua);
            {
                let _inner = Drawing::push(Id::Aqua);
                assert_eq!(current_look(), Look::Light);
            }
            assert_eq!(current(), Id::DarkAqua);
        }
        assert_eq!(TOP.with(Cell::get), None);
    }
}
