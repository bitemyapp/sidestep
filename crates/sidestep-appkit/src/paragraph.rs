//! `NSParagraphStyle` and `NSMutableParagraphStyle`: alignment, line
//! breaking, spacing, indents, line heights, base direction and tab stops,
//! as `text::layout` applies them; and `NSTextTab`.
//!
//! The mutable class is a subclass that adds setters over the same storage.
//! `copy` of a mutable style makes an immutable one, `mutableCopy` of either
//! a mutable one, and two styles are equal when all their values are.
//! Values read back exactly as they were set. Tab stops keep the order they
//! were set in, as AppKit's do (a tab goes to the first stop in the list
//! beyond it); `addTabStop:` puts a stop after the last one at or before
//! it (first if there is none), and `removeTabStop:` takes out the first
//! equal to it. `tabStops` hands back the `NSTextTab`s given. Stops compare
//! as `NSTextTab`s do: by location, alignment and whether they line up
//! decimal points, all exactly.

use std::cell::{OnceCell, Ref, RefCell};
use std::hash::Hasher;
use std::sync::Arc;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSLineBreakMode, NSLineBreakStrategy, NSMutableParagraphStyle, NSParagraphStyle,
    NSTabColumnTerminatorsAttributeName, NSTextAlignment, NSTextTab, NSTextTabType, NSWritingDirection,
};
use objc2_foundation::{NSArray, NSCopying, NSDictionary, NSString, NSZone};

use crate::text::layout::{Align, DEFAULT_TABS, Direction, LineBreak, Paragraph, Tab, TabKind};

/// Everything a paragraph style holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Style {
    pub layout: Paragraph,
    /// The tab stops as set, in that order; `None` is the default twelve.
    /// `layout.tabs` is made from them.
    stops: Option<Arc<[SetStop]>>,
    hyphenation_factor: f32,
    uses_default_hyphenation: bool,
    tightening_for_truncation: bool,
    tightening_factor: f32,
    line_break_strategy: usize,
    header_level: isize,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            layout: Paragraph::default(),
            stops: None,
            hyphenation_factor: 0.0,
            uses_default_hyphenation: false,
            // On by default in current macOS.
            tightening_for_truncation: true,
            tightening_factor: 0.0,
            line_break_strategy: 0,
            header_level: 0,
        }
    }
}

impl Style {
    /// The tab stops, the default ones spelled out.
    fn stops(&self) -> Vec<SetStop> {
        match &self.stops {
            Some(stops) => stops.to_vec(),
            None => {
                DEFAULT_TABS.iter().map(|t| SetStop { stop: Stop::left(f64::from(t.location)), tab: None }).collect()
            }
        }
    }

    /// Set the tab stops; `None` restores the default ones.
    fn set_stops(&mut self, stops: Option<Vec<SetStop>>) {
        // The default list, spelled out, is the default: it compares equal
        // and needs no allocation.
        let default =
            |s: &[SetStop]| s.len() == DEFAULT_TABS.len() && s.iter().zip(&DEFAULT_TABS).all(|(s, t)| s.is_default(t));
        self.stops = stops.filter(|s| !default(s)).map(Arc::from);
        self.layout.tabs = self.stops.as_ref().map(|s| s.iter().map(|s| s.stop.tab()).collect());
    }
}

/// A tab stop as it was set: its values, and the `NSTextTab` it was given
/// as, which `tabStops` hands back. Stops compare by their values alone,
/// without messages.
#[derive(Clone, Debug)]
struct SetStop {
    stop: Stop,
    tab: Option<Retained<NSTextTab>>,
}

impl PartialEq for SetStop {
    fn eq(&self, other: &Self) -> bool {
        self.stop == other.stop
    }
}

impl SetStop {
    fn of(tab: &NSTextTab) -> SetStop {
        SetStop { stop: tab_imp(tab).stop(), tab: Some(tab.retain()) }
    }

    fn is_default(&self, tab: &Tab) -> bool {
        self.stop == Stop::left(f64::from(tab.location))
    }

    /// The `NSTextTab` the stop was given as, or a new one.
    fn text_tab(self) -> Retained<NSTextTab> {
        self.tab.unwrap_or_else(|| {
            crate::load_shell::<NSTextTab>();
            let this = NSTextTabImpl::alloc().set_ivars(TabIvars { stop: self.stop, options: None });
            // SAFETY: NSObject's designated initializer.
            let tab: Retained<NSTextTabImpl> = unsafe { msg_send![super(this), init] };
            // SAFETY: NSTextTabImpl is NSTextTab's implementation.
            unsafe { Retained::cast_unchecked(tab) }
        })
    }
}

/// A tab stop as `NSTextTab` holds it, compared as it compares tabs.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stop {
    location: f64,
    alignment: NSTextAlignment,
    /// Lines up a decimal point: made by type, or with column terminators.
    decimal: bool,
}

impl Stop {
    fn left(location: f64) -> Stop {
        Stop { location, alignment: NSTextAlignment::Left, decimal: false }
    }

    /// The stop as layout uses it.
    fn tab(&self) -> Tab {
        let kind = if self.decimal {
            TabKind::Decimal
        } else if self.alignment == NSTextAlignment::Right {
            TabKind::Right
        } else if self.alignment == NSTextAlignment::Center {
            TabKind::Center
        } else {
            TabKind::Left
        };
        Tab { location: self.location as f32, kind }
    }

    /// Equal stops hash alike: equal locations have the same whole part.
    fn hash(&self) -> usize {
        (self.location as i64 as usize) ^ ((self.alignment.0 as usize) << 27) ^ (usize::from(self.decimal) << 26)
    }
}

pub(crate) struct StyleIvars {
    style: RefCell<Style>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSParagraphStyle"]
    #[ivars = StyleIvars]
    pub(crate) struct NSParagraphStyleImpl;

    impl NSParagraphStyleImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(StyleIvars { style: RefCell::default() });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(defaultParagraphStyle))]
        fn default_paragraph_style() -> Retained<Self> {
            // One per thread (styles aren't shared between threads), made
            // once: attributes without a style fall back to it often.
            thread_local!(static DEFAULT: OnceCell<Retained<NSParagraphStyleImpl>> = const { OnceCell::new() });
            DEFAULT.with(|d| d.get_or_init(|| new(Style::default())).clone())
        }

        #[unsafe(method(defaultWritingDirectionForLanguage:))]
        fn default_writing_direction(language: Option<&NSString>) -> NSWritingDirection {
            match language {
                Some(language) if right_to_left(&language.to_string()) => NSWritingDirection::RightToLeft,
                _ => NSWritingDirection::LeftToRight,
            }
        }

        #[unsafe(method(alignment))]
        fn alignment(&self) -> NSTextAlignment {
            match self.get().layout.alignment {
                Align::Left => NSTextAlignment::Left,
                Align::Right => NSTextAlignment::Right,
                Align::Center => NSTextAlignment::Center,
                Align::Justified => NSTextAlignment::Justified,
                Align::Natural => NSTextAlignment::Natural,
            }
        }

        #[unsafe(method(lineBreakMode))]
        fn line_break_mode(&self) -> NSLineBreakMode {
            match self.get().layout.line_break {
                LineBreak::WordWrap => NSLineBreakMode::ByWordWrapping,
                LineBreak::CharWrap => NSLineBreakMode::ByCharWrapping,
                LineBreak::Clip => NSLineBreakMode::ByClipping,
                LineBreak::TruncateHead => NSLineBreakMode::ByTruncatingHead,
                LineBreak::TruncateTail => NSLineBreakMode::ByTruncatingTail,
                LineBreak::TruncateMiddle => NSLineBreakMode::ByTruncatingMiddle,
            }
        }

        #[unsafe(method(baseWritingDirection))]
        fn base_writing_direction(&self) -> NSWritingDirection {
            match self.get().layout.direction {
                Direction::Natural => NSWritingDirection::Natural,
                Direction::LeftToRight => NSWritingDirection::LeftToRight,
                Direction::RightToLeft => NSWritingDirection::RightToLeft,
            }
        }

        #[unsafe(method(lineSpacing))]
        fn line_spacing(&self) -> f64 {
            self.get().layout.line_spacing
        }

        #[unsafe(method(paragraphSpacing))]
        fn paragraph_spacing(&self) -> f64 {
            self.get().layout.paragraph_spacing
        }

        #[unsafe(method(paragraphSpacingBefore))]
        fn paragraph_spacing_before(&self) -> f64 {
            self.get().layout.paragraph_spacing_before
        }

        #[unsafe(method(headIndent))]
        fn head_indent(&self) -> f64 {
            self.get().layout.head_indent
        }

        #[unsafe(method(firstLineHeadIndent))]
        fn first_line_head_indent(&self) -> f64 {
            self.get().layout.first_line_head_indent
        }

        #[unsafe(method(tailIndent))]
        fn tail_indent(&self) -> f64 {
            self.get().layout.tail_indent
        }

        #[unsafe(method(minimumLineHeight))]
        fn minimum_line_height(&self) -> f64 {
            self.get().layout.min_line_height
        }

        #[unsafe(method(maximumLineHeight))]
        fn maximum_line_height(&self) -> f64 {
            self.get().layout.max_line_height
        }

        #[unsafe(method(lineHeightMultiple))]
        fn line_height_multiple(&self) -> f64 {
            self.get().layout.line_height_multiple
        }

        #[unsafe(method(defaultTabInterval))]
        fn default_tab_interval(&self) -> f64 {
            self.get().layout.default_tab_interval
        }

        #[unsafe(method(hyphenationFactor))]
        fn hyphenation_factor(&self) -> f32 {
            self.get().hyphenation_factor
        }

        #[unsafe(method(usesDefaultHyphenation))]
        fn uses_default_hyphenation(&self) -> bool {
            self.get().uses_default_hyphenation
        }

        #[unsafe(method(allowsDefaultTighteningForTruncation))]
        fn allows_default_tightening(&self) -> bool {
            self.get().tightening_for_truncation
        }

        #[unsafe(method(tighteningFactorForTruncation))]
        fn tightening_factor(&self) -> f32 {
            self.get().tightening_factor
        }

        #[unsafe(method(lineBreakStrategy))]
        fn line_break_strategy(&self) -> NSLineBreakStrategy {
            NSLineBreakStrategy(self.get().line_break_strategy)
        }

        #[unsafe(method(headerLevel))]
        fn header_level(&self) -> isize {
            self.get().header_level
        }

        #[unsafe(method_id(tabStops))]
        fn tab_stops(&self) -> Retained<NSArray<NSTextTab>> {
            // Copied out first: making tabs sends messages.
            let stops = self.get().stops();
            let tabs: Vec<Retained<NSTextTab>> = stops.into_iter().map(SetStop::text_tab).collect();
            NSArray::from_retained_slice(&tabs)
        }

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut Self {
            // Immutable: a copy is the same style.
            Retained::into_raw(self.retain())
        }

        #[unsafe(method(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> *mut NSMutableParagraphStyle {
            let copy = NSMutableParagraphStyle::new();
            let style = self.get().clone();
            // Replaced, not assigned in a borrow: see `update`.
            drop(imp(&copy).ivars().style.replace(style));
            Retained::into_raw(copy)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSParagraphStyle>()).is_some_and(|o| *imp(o).get() == *self.get())
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            let style = self.get();
            let mut h = crate::text::layout::Fx::default();
            style.layout.hash_into(&mut h);
            for set in style.stops.iter().flat_map(|s| s.iter()) {
                h.write_usize(set.stop.hash());
            }
            h.finish() as usize
        }
    }

    unsafe impl NSObjectProtocol for NSParagraphStyleImpl {}
);

define_class!(
    #[unsafe(super(NSParagraphStyle, NSObject))]
    #[name = "NSMutableParagraphStyle"]
    pub(crate) struct NSMutableParagraphStyleImpl;

    impl NSMutableParagraphStyleImpl {
        #[unsafe(method(setAlignment:))]
        fn set_alignment(&self, alignment: NSTextAlignment) {
            let alignment = if alignment == NSTextAlignment::Left {
                Align::Left
            } else if alignment == NSTextAlignment::Right {
                Align::Right
            } else if alignment == NSTextAlignment::Center {
                Align::Center
            } else if alignment == NSTextAlignment::Justified {
                Align::Justified
            } else {
                Align::Natural
            };
            self.update(|s| s.layout.alignment = alignment);
        }

        #[unsafe(method(setLineBreakMode:))]
        fn set_line_break_mode(&self, mode: NSLineBreakMode) {
            let mode = match mode {
                NSLineBreakMode::ByCharWrapping => LineBreak::CharWrap,
                NSLineBreakMode::ByClipping => LineBreak::Clip,
                NSLineBreakMode::ByTruncatingHead => LineBreak::TruncateHead,
                NSLineBreakMode::ByTruncatingTail => LineBreak::TruncateTail,
                NSLineBreakMode::ByTruncatingMiddle => LineBreak::TruncateMiddle,
                _ => LineBreak::WordWrap,
            };
            self.update(|s| s.layout.line_break = mode);
        }

        #[unsafe(method(setBaseWritingDirection:))]
        fn set_base_writing_direction(&self, direction: NSWritingDirection) {
            let direction = match direction {
                NSWritingDirection::LeftToRight => Direction::LeftToRight,
                NSWritingDirection::RightToLeft => Direction::RightToLeft,
                _ => Direction::Natural,
            };
            self.update(|s| s.layout.direction = direction);
        }

        #[unsafe(method(setLineSpacing:))]
        fn set_line_spacing(&self, value: f64) {
            self.update(|s| s.layout.line_spacing = value);
        }

        #[unsafe(method(setParagraphSpacing:))]
        fn set_paragraph_spacing(&self, value: f64) {
            self.update(|s| s.layout.paragraph_spacing = value);
        }

        #[unsafe(method(setParagraphSpacingBefore:))]
        fn set_paragraph_spacing_before(&self, value: f64) {
            self.update(|s| s.layout.paragraph_spacing_before = value);
        }

        #[unsafe(method(setHeadIndent:))]
        fn set_head_indent(&self, value: f64) {
            self.update(|s| s.layout.head_indent = value);
        }

        #[unsafe(method(setFirstLineHeadIndent:))]
        fn set_first_line_head_indent(&self, value: f64) {
            self.update(|s| s.layout.first_line_head_indent = value);
        }

        #[unsafe(method(setTailIndent:))]
        fn set_tail_indent(&self, value: f64) {
            self.update(|s| s.layout.tail_indent = value);
        }

        #[unsafe(method(setMinimumLineHeight:))]
        fn set_minimum_line_height(&self, value: f64) {
            self.update(|s| s.layout.min_line_height = value);
        }

        #[unsafe(method(setMaximumLineHeight:))]
        fn set_maximum_line_height(&self, value: f64) {
            self.update(|s| s.layout.max_line_height = value);
        }

        #[unsafe(method(setLineHeightMultiple:))]
        fn set_line_height_multiple(&self, value: f64) {
            self.update(|s| s.layout.line_height_multiple = value);
        }

        #[unsafe(method(setDefaultTabInterval:))]
        fn set_default_tab_interval(&self, value: f64) {
            self.update(|s| s.layout.default_tab_interval = value);
        }

        #[unsafe(method(setHyphenationFactor:))]
        fn set_hyphenation_factor(&self, value: f32) {
            self.update(|s| s.hyphenation_factor = value);
        }

        #[unsafe(method(setUsesDefaultHyphenation:))]
        fn set_uses_default_hyphenation(&self, value: bool) {
            self.update(|s| s.uses_default_hyphenation = value);
        }

        #[unsafe(method(setAllowsDefaultTighteningForTruncation:))]
        fn set_allows_default_tightening(&self, value: bool) {
            self.update(|s| s.tightening_for_truncation = value);
        }

        #[unsafe(method(setTighteningFactorForTruncation:))]
        fn set_tightening_factor(&self, value: f32) {
            self.update(|s| s.tightening_factor = value);
        }

        #[unsafe(method(setLineBreakStrategy:))]
        fn set_line_break_strategy(&self, value: NSLineBreakStrategy) {
            self.update(|s| s.line_break_strategy = value.0);
        }

        #[unsafe(method(setHeaderLevel:))]
        fn set_header_level(&self, value: isize) {
            self.update(|s| s.header_level = value);
        }

        #[unsafe(method(setTabStops:))]
        fn set_tab_stops(&self, tabs: Option<&NSArray<NSTextTab>>) {
            // nil restores the default stops; an empty array leaves none.
            let stops = tabs.map(|tabs| {
                let items = crate::font::array_items(tabs);
                items.iter().filter_map(|t| t.downcast_ref::<NSTextTab>()).map(SetStop::of).collect()
            });
            self.update(|s| s.set_stops(stops));
        }

        #[unsafe(method(addTabStop:))]
        fn add_tab_stop(&self, tab: &NSTextTab) {
            let set = SetStop::of(tab);
            self.update(|s| {
                let mut stops = s.stops();
                // After the last stop at or before it, or first.
                let at = stops.iter().rposition(|t| t.stop.location <= set.stop.location).map_or(0, |i| i + 1);
                stops.insert(at, set);
                s.set_stops(Some(stops));
            });
        }

        #[unsafe(method(removeTabStop:))]
        fn remove_tab_stop(&self, tab: &NSTextTab) {
            let stop = tab_imp(tab).stop();
            self.update(|s| {
                let mut stops = s.stops();
                if let Some(at) = stops.iter().position(|t| t.stop == stop) {
                    stops.remove(at);
                    s.set_stops(Some(stops));
                }
            });
        }

        #[unsafe(method(setParagraphStyle:))]
        fn set_paragraph_style(&self, other: &NSParagraphStyle) {
            let style = imp(other).get().clone();
            self.update(|s| *s = style);
        }

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut NSParagraphStyle {
            let copy = new(self.base().get().clone());
            // SAFETY: NSParagraphStyleImpl is NSParagraphStyle's implementation.
            Retained::into_raw(unsafe { Retained::cast_unchecked(copy) })
        }
    }
);

impl NSParagraphStyleImpl {
    fn get(&self) -> Ref<'_, Style> {
        self.ivars().style.borrow()
    }
}

impl NSMutableParagraphStyleImpl {
    fn base(&self) -> &NSParagraphStyleImpl {
        // SAFETY: the mutable class is a subclass, sharing its storage.
        unsafe { &*(self as *const Self).cast::<NSParagraphStyleImpl>() }
    }

    /// Change the style: `f` changes a copy (sharing the tab stops), which
    /// then takes the style's place. The old style goes once the style is
    /// no longer borrowed, since it may hold the last reference to a tab
    /// whose dealloc (an app's subclass, say) reads this style.
    fn update(&self, f: impl FnOnce(&mut Style)) {
        let cell = &self.base().ivars().style;
        let mut next = cell.borrow().clone();
        f(&mut next);
        drop(cell.replace(next));
    }
}

fn new(style: Style) -> Retained<NSParagraphStyleImpl> {
    crate::load_shell::<objc2_app_kit::NSParagraphStyle>();
    let this = NSParagraphStyleImpl::alloc().set_ivars(StyleIvars { style: RefCell::new(style) });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

pub(crate) fn imp(style: &NSParagraphStyle) -> &NSParagraphStyleImpl {
    // SAFETY: NSParagraphStyle is NSParagraphStyleImpl's class, and the
    // mutable subclass shares its layout.
    unsafe { &*(style as *const NSParagraphStyle).cast::<NSParagraphStyleImpl>() }
}

/// What layout needs of a paragraph style.
pub(crate) fn paragraph_of(style: &NSParagraphStyle) -> Paragraph {
    imp(style).get().layout.clone()
}

/// Whether text in `language` (a BCP 47 or ICU code) runs right to left:
/// languages written in the Arabic, Hebrew, Thaana, Syriac or N'Ko scripts
/// by default, and any language asked for in one of those scripts.
fn right_to_left(language: &str) -> bool {
    const LANGUAGES: &[&str] = &[
        "ar", "ckb", "dv", "fa", "he", "iw", "ji", "ks", "lrc", "mzn", "nqo", "ps", "sd", "sdh", "syr", "ug", "ur",
        "yi",
    ];
    const SCRIPTS: &[&str] = &["arab", "hebr", "thaa", "syrc", "nkoo", "adlm", "rohg"];
    let mut parts = language.split(['-', '_']);
    let primary = parts.next().unwrap_or_default();
    LANGUAGES.iter().any(|l| l.eq_ignore_ascii_case(primary))
        || parts.any(|p| p.len() == 4 && SCRIPTS.iter().any(|s| s.eq_ignore_ascii_case(p)))
}

// NSTextTab

pub(crate) struct TabIvars {
    stop: Stop,
    options: Option<Retained<NSDictionary<NSString, AnyObject>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextTab"]
    #[ivars = TabIvars]
    pub(crate) struct NSTextTabImpl;

    impl NSTextTabImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(TabIvars { stop: Stop::left(0.0), options: None });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithTextAlignment:location:options:))]
        fn init_with_alignment(
            this: Allocated<Self>,
            alignment: NSTextAlignment,
            location: f64,
            options: &NSDictionary<NSString, AnyObject>,
        ) -> Retained<Self> {
            // SAFETY: the key is this crate's own constant.
            let decimal = options.objectForKey(unsafe { NSTabColumnTerminatorsAttributeName }).is_some();
            let options = Some(options.copy());
            let this = this.set_ivars(TabIvars { stop: Stop { location, alignment, decimal }, options });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithType:location:))]
        fn init_with_type(this: Allocated<Self>, kind: NSTextTabType, location: f64) -> Retained<Self> {
            let (alignment, decimal) = match kind {
                NSTextTabType::RightTabStopType => (NSTextAlignment::Right, false),
                NSTextTabType::CenterTabStopType => (NSTextAlignment::Center, false),
                NSTextTabType::DecimalTabStopType => (NSTextAlignment::Natural, true),
                _ => (NSTextAlignment::Left, false),
            };
            let this = this.set_ivars(TabIvars { stop: Stop { location, alignment, decimal }, options: None });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(location))]
        fn location(&self) -> f64 {
            self.ivars().stop.location
        }

        #[unsafe(method(alignment))]
        fn alignment(&self) -> NSTextAlignment {
            self.ivars().stop.alignment
        }

        #[unsafe(method(tabStopType))]
        fn tab_stop_type(&self) -> NSTextTabType {
            match self.stop().tab().kind {
                TabKind::Left => NSTextTabType::LeftTabStopType,
                TabKind::Right => NSTextTabType::RightTabStopType,
                TabKind::Center => NSTextTabType::CenterTabStopType,
                TabKind::Decimal => NSTextTabType::DecimalTabStopType,
            }
        }

        #[unsafe(method_id(options))]
        fn options(&self) -> Retained<NSDictionary<NSString, AnyObject>> {
            self.ivars().options.clone().unwrap_or_default()
        }

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut Self {
            // Immutable: a copy is the same tab.
            Retained::into_raw(self.retain())
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSTextTab>()).is_some_and(|o| tab_imp(o).stop() == self.stop())
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            self.stop().hash()
        }
    }

    unsafe impl NSObjectProtocol for NSTextTabImpl {}
);

impl NSTextTabImpl {
    fn stop(&self) -> Stop {
        self.ivars().stop
    }
}

fn tab_imp(tab: &NSTextTab) -> &NSTextTabImpl {
    // SAFETY: every NSTextTab is an NSTextTabImpl.
    unsafe { &*(tab as *const NSTextTab).cast::<NSTextTabImpl>() }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use objc2::rc::{Retained, autoreleasepool};
    use objc2::{AnyThread, define_class, msg_send};
    use objc2_app_kit::{NSMutableParagraphStyle, NSTextTab, NSTextTabType};
    use objc2_foundation::NSArray;

    thread_local! {
        static STYLE: Cell<Option<*const NSMutableParagraphStyle>> = const { Cell::new(None) };
        static READ: Cell<usize> = const { Cell::new(0) };
    }

    /// When its tab goes, it reads the style the test names, as an app's
    /// tab subclass could in its dealloc.
    struct Reader;

    impl Drop for Reader {
        fn drop(&mut self) {
            if let Some(style) = STYLE.take() {
                // SAFETY: the test keeps the style alive while it names it.
                READ.set(unsafe { &*style }.tabStops().count());
            }
        }
    }

    define_class!(
        #[unsafe(super(NSTextTab, objc2::runtime::NSObject))]
        #[name = "SidestepTestReadingTab"]
        #[ivars = Reader]
        struct ReadingTab;
    );

    #[test]
    fn a_tab_released_by_its_style_can_read_it() {
        let style = NSMutableParagraphStyle::new();
        autoreleasepool(|_| {
            let tab = ReadingTab::alloc().set_ivars(Reader);
            // SAFETY: NSTextTab's initializer.
            let tab: Retained<ReadingTab> =
                unsafe { msg_send![super(tab), initWithType: NSTextTabType::LeftTabStopType, location: 40.0] };
            style.setTabStops(Some(&NSArray::from_retained_slice(&[Retained::into_super(tab)])));
            assert_eq!(style.tabStops().count(), 1);
        });
        // The style holds the tab's last reference; replacing the stops
        // releases it, and its dealloc reads the style, which isn't
        // borrowed by then.
        STYLE.set(Some(&*style));
        autoreleasepool(|_| style.setTabStops(None));
        assert_eq!(STYLE.take(), None, "the tab went");
        assert_eq!(READ.get(), 12, "and found the default stops");
    }
}
