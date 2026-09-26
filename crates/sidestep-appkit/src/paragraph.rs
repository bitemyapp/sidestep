//! `NSParagraphStyle` and `NSMutableParagraphStyle`: alignment, line
//! breaking, spacing, indents, line heights and base direction, as
//! `text::layout` applies them.
//!
//! The mutable class is a subclass that adds setters over the same storage.
//! `copy` of a mutable style makes an immutable one, `mutableCopy` of either
//! a mutable one, and two styles are equal when all their values are.
//! Tab stops (`NSTextTab`) can be set and added; reading them back as an
//! array waits for Foundation's `NSArray`.

use std::cell::Cell;
use std::hash::Hasher;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSLineBreakMode, NSLineBreakStrategy, NSMutableParagraphStyle, NSParagraphStyle,
    NSTabColumnTerminatorsAttributeName, NSTextAlignment, NSTextTab, NSTextTabType, NSWritingDirection,
};
use objc2_foundation::{NSArray, NSCopying, NSDictionary, NSString, NSZone};

use crate::text::layout::{Align, Direction, LineBreak, Paragraph, Tab, TabKind, intern_tabs, tab_list};

/// Everything a paragraph style holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Style {
    pub layout: Paragraph,
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
            hyphenation_factor: 0.0,
            uses_default_hyphenation: false,
            tightening_for_truncation: false,
            tightening_factor: 0.0,
            line_break_strategy: 0,
            header_level: 0,
        }
    }
}

pub(crate) struct StyleIvars {
    style: Cell<Style>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSParagraphStyle"]
    #[ivars = StyleIvars]
    pub(crate) struct NSParagraphStyleImpl;

    impl NSParagraphStyleImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(StyleIvars { style: Cell::new(Style::default()) });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(defaultParagraphStyle))]
        fn default_paragraph_style() -> Retained<Self> {
            new(Style::default())
        }

        #[unsafe(method(defaultWritingDirectionForLanguage:))]
        fn default_writing_direction(_language: Option<&AnyObject>) -> NSWritingDirection {
            NSWritingDirection::LeftToRight
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
            self.get().layout.line_spacing.into()
        }

        #[unsafe(method(paragraphSpacing))]
        fn paragraph_spacing(&self) -> f64 {
            self.get().layout.paragraph_spacing.into()
        }

        #[unsafe(method(paragraphSpacingBefore))]
        fn paragraph_spacing_before(&self) -> f64 {
            self.get().layout.paragraph_spacing_before.into()
        }

        #[unsafe(method(headIndent))]
        fn head_indent(&self) -> f64 {
            self.get().layout.head_indent.into()
        }

        #[unsafe(method(firstLineHeadIndent))]
        fn first_line_head_indent(&self) -> f64 {
            self.get().layout.first_line_head_indent.into()
        }

        #[unsafe(method(tailIndent))]
        fn tail_indent(&self) -> f64 {
            self.get().layout.tail_indent.into()
        }

        #[unsafe(method(minimumLineHeight))]
        fn minimum_line_height(&self) -> f64 {
            self.get().layout.min_line_height.into()
        }

        #[unsafe(method(maximumLineHeight))]
        fn maximum_line_height(&self) -> f64 {
            self.get().layout.max_line_height.into()
        }

        #[unsafe(method(lineHeightMultiple))]
        fn line_height_multiple(&self) -> f64 {
            self.get().layout.line_height_multiple.into()
        }

        #[unsafe(method(defaultTabInterval))]
        fn default_tab_interval(&self) -> f64 {
            self.get().layout.default_tab_interval.into()
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

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut Self {
            // Immutable: a copy is the same style.
            Retained::into_raw(self.retain())
        }

        #[unsafe(method(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> *mut NSMutableParagraphStyle {
            let copy = NSMutableParagraphStyle::new();
            imp(&copy).ivars().style.set(self.get());
            Retained::into_raw(copy)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSParagraphStyle>()).is_some_and(|o| imp(o).get() == self.get())
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            let mut h = crate::text::layout::Fx::default();
            self.get().layout.hash_into(&mut h);
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
            self.update(|s| s.layout.line_spacing = value as f32);
        }

        #[unsafe(method(setParagraphSpacing:))]
        fn set_paragraph_spacing(&self, value: f64) {
            self.update(|s| s.layout.paragraph_spacing = value as f32);
        }

        #[unsafe(method(setParagraphSpacingBefore:))]
        fn set_paragraph_spacing_before(&self, value: f64) {
            self.update(|s| s.layout.paragraph_spacing_before = value as f32);
        }

        #[unsafe(method(setHeadIndent:))]
        fn set_head_indent(&self, value: f64) {
            self.update(|s| s.layout.head_indent = value as f32);
        }

        #[unsafe(method(setFirstLineHeadIndent:))]
        fn set_first_line_head_indent(&self, value: f64) {
            self.update(|s| s.layout.first_line_head_indent = value as f32);
        }

        #[unsafe(method(setTailIndent:))]
        fn set_tail_indent(&self, value: f64) {
            self.update(|s| s.layout.tail_indent = value as f32);
        }

        #[unsafe(method(setMinimumLineHeight:))]
        fn set_minimum_line_height(&self, value: f64) {
            self.update(|s| s.layout.min_line_height = value as f32);
        }

        #[unsafe(method(setMaximumLineHeight:))]
        fn set_maximum_line_height(&self, value: f64) {
            self.update(|s| s.layout.max_line_height = value as f32);
        }

        #[unsafe(method(setLineHeightMultiple:))]
        fn set_line_height_multiple(&self, value: f64) {
            self.update(|s| s.layout.line_height_multiple = value as f32);
        }

        #[unsafe(method(setDefaultTabInterval:))]
        fn set_default_tab_interval(&self, value: f64) {
            self.update(|s| s.layout.default_tab_interval = value as f32);
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
            let items = tabs.map(|t| crate::font::array_items(t)).unwrap_or_default();
            let tabs: Vec<Tab> = items.iter().filter_map(|t| t.downcast_ref::<NSTextTab>()).map(|t| tab_imp(t).tab()).collect();
            self.set_tabs(tabs);
        }

        #[unsafe(method(addTabStop:))]
        fn add_tab_stop(&self, tab: &NSTextTab) {
            let mut tabs = tab_list(self.base().get().layout.tabs).to_vec();
            tabs.push(tab_imp(tab).tab());
            self.set_tabs(tabs);
        }

        #[unsafe(method(removeTabStop:))]
        fn remove_tab_stop(&self, tab: &NSTextTab) {
            let tab = tab_imp(tab).tab();
            let mut tabs = tab_list(self.base().get().layout.tabs).to_vec();
            tabs.retain(|t| *t != tab);
            self.set_tabs(tabs);
        }

        #[unsafe(method(setParagraphStyle:))]
        fn set_paragraph_style(&self, other: &NSParagraphStyle) {
            let style = imp(other).get();
            self.update(|s| *s = style);
        }

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut NSParagraphStyle {
            let copy = new(self.base().get());
            // SAFETY: NSParagraphStyleImpl is NSParagraphStyle's implementation.
            Retained::into_raw(unsafe { Retained::cast_unchecked(copy) })
        }
    }
);

impl NSParagraphStyleImpl {
    fn get(&self) -> Style {
        self.ivars().style.get()
    }
}

impl NSMutableParagraphStyleImpl {
    fn base(&self) -> &NSParagraphStyleImpl {
        // SAFETY: the mutable class is a subclass, sharing its storage.
        unsafe { &*(self as *const Self).cast::<NSParagraphStyleImpl>() }
    }

    fn set_tabs(&self, mut tabs: Vec<Tab>) {
        tabs.sort_by(|a, b| a.location.total_cmp(&b.location));
        let id = intern_tabs(&tabs);
        self.update(|s| s.layout.tabs = id);
    }

    fn update(&self, f: impl FnOnce(&mut Style)) {
        let cell = &self.base().ivars().style;
        let mut style = cell.get();
        f(&mut style);
        cell.set(style);
    }
}

fn new(style: Style) -> Retained<NSParagraphStyleImpl> {
    let this = NSParagraphStyleImpl::alloc().set_ivars(StyleIvars { style: Cell::new(style) });
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
    imp(style).get().layout
}

// NSTextTab

pub(crate) struct TabIvars {
    location: f64,
    alignment: NSTextAlignment,
    /// Made as a decimal tab: by type, or with column terminators.
    decimal: bool,
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
            let this =
                this.set_ivars(TabIvars { location: 0.0, alignment: NSTextAlignment::Left, decimal: false, options: None });
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
            let this = this.set_ivars(TabIvars { location, alignment, decimal, options });
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
            let this = this.set_ivars(TabIvars { location, alignment, decimal, options: None });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(location))]
        fn location(&self) -> f64 {
            self.ivars().location
        }

        #[unsafe(method(alignment))]
        fn alignment(&self) -> NSTextAlignment {
            self.ivars().alignment
        }

        #[unsafe(method(tabStopType))]
        fn tab_stop_type(&self) -> NSTextTabType {
            match self.tab().kind {
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
            other.and_then(|o| o.downcast_ref::<NSTextTab>()).is_some_and(|o| tab_imp(o).tab() == self.tab())
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            self.ivars().location.to_bits() as usize
        }
    }

    unsafe impl NSObjectProtocol for NSTextTabImpl {}
);

impl NSTextTabImpl {
    fn tab(&self) -> Tab {
        let ivars = self.ivars();
        let kind = if ivars.decimal {
            TabKind::Decimal
        } else if ivars.alignment == NSTextAlignment::Right {
            TabKind::Right
        } else if ivars.alignment == NSTextAlignment::Center {
            TabKind::Center
        } else {
            TabKind::Left
        };
        Tab { location: ivars.location as f32, kind }
    }
}

fn tab_imp(tab: &NSTextTab) -> &NSTextTabImpl {
    // SAFETY: every NSTextTab is an NSTextTabImpl.
    unsafe { &*(tab as *const NSTextTab).cast::<NSTextTabImpl>() }
}
