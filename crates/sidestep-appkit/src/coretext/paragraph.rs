//! `CTParagraphStyle` and `CTTextTab`, and the small types: glyph infos,
//! run delegates and ruby annotations (kept and handed back; layout
//! doesn't use them).
//!
//! A paragraph style keeps the settings it was made with, reports each
//! (or its default) through `CTParagraphStyleGetValueForSpecifier`, and
//! gives the text engine the paragraph they describe: alignment, indents,
//! line breaking, line heights and spacing, the base direction and tab
//! stops. It's a type of its own, as on macOS, under the same attribute
//! name as AppKit's paragraph style (`NSParagraphStyle`), which CoreText
//! reads too.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::{CFDictionary, CFString, CFTypeID, CGFloat};
use objc2_core_graphics::{CGFontIndex, CGGlyph};
use objc2_core_text::{
    CTCharacterCollection, CTFont, CTGlyphInfo, CTParagraphStyle, CTParagraphStyleSetting, CTParagraphStyleSpecifier,
    CTRubyAlignment, CTRubyAnnotation, CTRubyOverhang, CTRubyPosition, CTRunDelegate, CTRunDelegateCallbacks,
    CTTextAlignment, CTTextTab,
};
use objc2_foundation::{NSArray, NSCopying, NSDictionary, NSString};

use super::{owned, text_of};
use crate::text::layout::{Align, Direction, LineBreak, Paragraph, Tab, TabKind};

/// A paragraph style's settings, CoreText's defaults where not given.
#[derive(Clone)]
pub(crate) struct Settings {
    alignment: u8,
    first_line_head_indent: f64,
    head_indent: f64,
    tail_indent: f64,
    tabs: Option<Retained<NSArray<AnyObject>>>,
    default_tab_interval: f64,
    line_break: u8,
    line_height_multiple: f64,
    maximum_line_height: f64,
    minimum_line_height: f64,
    line_spacing: f64,
    paragraph_spacing: f64,
    paragraph_spacing_before: f64,
    direction: i8,
    maximum_line_spacing: f64,
    minimum_line_spacing: f64,
    line_spacing_adjustment: f64,
    line_bounds_options: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            alignment: CTTextAlignment::Natural.0,
            first_line_head_indent: 0.0,
            head_indent: 0.0,
            tail_indent: 0.0,
            tabs: None,
            default_tab_interval: 0.0,
            line_break: 0,
            line_height_multiple: 0.0,
            maximum_line_height: 0.0,
            minimum_line_height: 0.0,
            line_spacing: 0.0,
            paragraph_spacing: 0.0,
            paragraph_spacing_before: 0.0,
            direction: -1,
            // What macOS reports for none set.
            maximum_line_spacing: 1e7,
            minimum_line_spacing: 0.0,
            line_spacing_adjustment: 0.0,
            line_bounds_options: 0,
        }
    }
}

pub(crate) struct ParagraphIvars {
    settings: Settings,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; styles are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTParagraphStyle"]
    #[ivars = ParagraphIvars]
    pub(crate) struct CTParagraphStyleImpl;

    impl CTParagraphStyleImpl {
        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut c_void) -> *mut Self {
            Retained::into_raw(self.retain())
        }
    }

    unsafe impl NSObjectProtocol for CTParagraphStyleImpl {}
);

impl CTParagraphStyleImpl {
    /// The paragraph the text engine lays out.
    pub(crate) fn layout(&self) -> Paragraph {
        let s = &self.ivars().settings;
        let tabs = s.tabs.as_ref().map(|tabs| {
            let stops: Vec<Tab> = tabs
                .iter()
                .filter_map(|t| t.downcast::<CTTextTabImpl>().ok())
                .map(|t| {
                    let kind = match t.ivars().alignment {
                        1 => TabKind::Right,
                        2 => TabKind::Center,
                        _ => TabKind::Left,
                    };
                    Tab { location: t.ivars().location as f32, kind }
                })
                .collect();
            Arc::<[Tab]>::from(stops)
        });
        Paragraph {
            alignment: match s.alignment {
                0 => Align::Left,
                1 => Align::Right,
                2 => Align::Center,
                3 => Align::Justified,
                _ => Align::Natural,
            },
            line_break: match s.line_break {
                1 => LineBreak::CharWrap,
                2 => LineBreak::Clip,
                3 => LineBreak::TruncateHead,
                4 => LineBreak::TruncateTail,
                5 => LineBreak::TruncateMiddle,
                _ => LineBreak::WordWrap,
            },
            line_spacing: s.line_spacing.max(s.minimum_line_spacing) + s.line_spacing_adjustment,
            paragraph_spacing: s.paragraph_spacing,
            paragraph_spacing_before: s.paragraph_spacing_before,
            head_indent: s.head_indent,
            first_line_head_indent: s.first_line_head_indent,
            tail_indent: s.tail_indent,
            min_line_height: s.minimum_line_height,
            max_line_height: s.maximum_line_height,
            line_height_multiple: s.line_height_multiple,
            direction: match s.direction {
                0 => Direction::LeftToRight,
                1 => Direction::RightToLeft,
                _ => Direction::Natural,
            },
            default_tab_interval: s.default_tab_interval,
            tabs,
        }
    }
}

fn style_imp(s: &CTParagraphStyle) -> &CTParagraphStyleImpl {
    // SAFETY: every CTParagraphStyle is a CTParagraphStyleImpl.
    unsafe { &*(s as *const CTParagraphStyle).cast::<CTParagraphStyleImpl>() }
}

fn new_style(settings: Settings) -> Retained<CTParagraphStyleImpl> {
    let this = CTParagraphStyleImpl::alloc().set_ivars(ParagraphIvars { settings });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTParagraphStyleGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_PARAGRAPH_STYLE
}

/// Read a setting's value: `size` bytes at `value`.
///
/// # Safety
///
/// `value` points at `size` readable bytes.
unsafe fn read<T: Copy>(value: *const c_void, size: usize) -> Option<T> {
    // SAFETY: as the caller promises, and the size is the type's.
    (size == std::mem::size_of::<T>() && !value.is_null()).then(|| unsafe { value.cast::<T>().read_unaligned() })
}

/// Settings of unknown specifiers or the wrong size are left out.
///
/// # Safety
///
/// `settings` holds `count` settings, each pointing at its value.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTParagraphStyleCreate(
    settings: *const CTParagraphStyleSetting,
    count: usize,
) -> Option<NonNull<CTParagraphStyle>> {
    let mut s = Settings::default();
    // SAFETY: as the caller promises.
    for setting in unsafe { crate::coregraphics::slice(settings, count) } {
        let (size, value) = (setting.valueSize, setting.value.as_ptr().cast_const());
        // SAFETY: each setting points at `size` bytes.
        unsafe {
            let float = || read::<CGFloat>(value, size);
            match setting.spec {
                CTParagraphStyleSpecifier::Alignment => s.alignment = read::<u8>(value, size).unwrap_or(s.alignment),
                CTParagraphStyleSpecifier::FirstLineHeadIndent => {
                    s.first_line_head_indent = float().unwrap_or(s.first_line_head_indent)
                }
                CTParagraphStyleSpecifier::HeadIndent => s.head_indent = float().unwrap_or(s.head_indent),
                CTParagraphStyleSpecifier::TailIndent => s.tail_indent = float().unwrap_or(s.tail_indent),
                CTParagraphStyleSpecifier::TabStops => {
                    if let Some(array) = read::<*const NSArray<AnyObject>>(value, size).and_then(|a| a.as_ref()) {
                        s.tabs = Some(array.copy());
                    }
                }
                CTParagraphStyleSpecifier::DefaultTabInterval => {
                    s.default_tab_interval = float().unwrap_or(s.default_tab_interval)
                }
                CTParagraphStyleSpecifier::LineBreakMode => {
                    s.line_break = read::<u8>(value, size).unwrap_or(s.line_break)
                }
                CTParagraphStyleSpecifier::LineHeightMultiple => {
                    s.line_height_multiple = float().unwrap_or(s.line_height_multiple)
                }
                CTParagraphStyleSpecifier::MaximumLineHeight => {
                    s.maximum_line_height = float().unwrap_or(s.maximum_line_height)
                }
                CTParagraphStyleSpecifier::MinimumLineHeight => {
                    s.minimum_line_height = float().unwrap_or(s.minimum_line_height)
                }
                #[allow(deprecated)]
                CTParagraphStyleSpecifier::LineSpacing => s.line_spacing = float().unwrap_or(s.line_spacing),
                CTParagraphStyleSpecifier::ParagraphSpacing => {
                    s.paragraph_spacing = float().unwrap_or(s.paragraph_spacing)
                }
                CTParagraphStyleSpecifier::ParagraphSpacingBefore => {
                    s.paragraph_spacing_before = float().unwrap_or(s.paragraph_spacing_before)
                }
                CTParagraphStyleSpecifier::BaseWritingDirection => {
                    s.direction = read::<i8>(value, size).unwrap_or(s.direction)
                }
                CTParagraphStyleSpecifier::MaximumLineSpacing => {
                    s.maximum_line_spacing = float().unwrap_or(s.maximum_line_spacing)
                }
                CTParagraphStyleSpecifier::MinimumLineSpacing => {
                    s.minimum_line_spacing = float().unwrap_or(s.minimum_line_spacing)
                }
                CTParagraphStyleSpecifier::LineSpacingAdjustment => {
                    s.line_spacing_adjustment = float().unwrap_or(s.line_spacing_adjustment)
                }
                CTParagraphStyleSpecifier::LineBoundsOptions => {
                    s.line_bounds_options = read::<usize>(value, size).unwrap_or(s.line_bounds_options)
                }
                _ => {}
            }
        }
    }
    Some(owned(new_style(s)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTParagraphStyleCreateCopy(
    style: Option<&CTParagraphStyle>,
) -> Option<NonNull<CTParagraphStyle>> {
    let settings = style.map_or_else(Settings::default, |s| style_imp(s).ivars().settings.clone());
    Some(owned(new_style(settings)))
}

/// The default tab stops: twelve left stops 28 points apart, made once per
/// thread and kept for good (a style hands them out unretained).
fn default_tabs() -> *const NSArray<AnyObject> {
    thread_local!(static TABS: *const NSArray<AnyObject> = {
        let tabs: Vec<Retained<AnyObject>> =
            (1..=12).map(|i| super::any(new_tab(0, 28.0 * f64::from(i), None))).collect();
        Retained::into_raw(NSArray::from_retained_slice(&tabs)).cast_const()
    });
    TABS.with(|t| *t)
}

/// # Safety
///
/// `buffer` has room for `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTParagraphStyleGetValueForSpecifier(
    style: Option<&CTParagraphStyle>,
    spec: CTParagraphStyleSpecifier,
    size: usize,
    buffer: *mut c_void,
) -> bool {
    let defaults = Settings::default();
    let s = style.map_or(&defaults, |s| &style_imp(s).ivars().settings);
    // SAFETY: as the caller promises.
    let write = |bytes: &[u8]| -> bool {
        if bytes.len() != size || buffer.is_null() {
            return false;
        }
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), size) };
        true
    };
    match spec {
        CTParagraphStyleSpecifier::Alignment => write(&[s.alignment]),
        CTParagraphStyleSpecifier::FirstLineHeadIndent => write(&s.first_line_head_indent.to_ne_bytes()),
        CTParagraphStyleSpecifier::HeadIndent => write(&s.head_indent.to_ne_bytes()),
        CTParagraphStyleSpecifier::TailIndent => write(&s.tail_indent.to_ne_bytes()),
        CTParagraphStyleSpecifier::TabStops => {
            // The style (or the thread) keeps the array; the caller gets it
            // unretained.
            let ptr = s.tabs.as_ref().map_or_else(default_tabs, Retained::as_ptr) as usize;
            write(&ptr.to_ne_bytes())
        }
        CTParagraphStyleSpecifier::DefaultTabInterval => write(&s.default_tab_interval.to_ne_bytes()),
        CTParagraphStyleSpecifier::LineBreakMode => write(&[s.line_break]),
        CTParagraphStyleSpecifier::LineHeightMultiple => write(&s.line_height_multiple.to_ne_bytes()),
        CTParagraphStyleSpecifier::MaximumLineHeight => write(&s.maximum_line_height.to_ne_bytes()),
        CTParagraphStyleSpecifier::MinimumLineHeight => write(&s.minimum_line_height.to_ne_bytes()),
        #[allow(deprecated)]
        CTParagraphStyleSpecifier::LineSpacing => write(&s.line_spacing.to_ne_bytes()),
        CTParagraphStyleSpecifier::ParagraphSpacing => write(&s.paragraph_spacing.to_ne_bytes()),
        CTParagraphStyleSpecifier::ParagraphSpacingBefore => write(&s.paragraph_spacing_before.to_ne_bytes()),
        CTParagraphStyleSpecifier::BaseWritingDirection => write(&s.direction.to_ne_bytes()),
        CTParagraphStyleSpecifier::MaximumLineSpacing => write(&s.maximum_line_spacing.to_ne_bytes()),
        CTParagraphStyleSpecifier::MinimumLineSpacing => write(&s.minimum_line_spacing.to_ne_bytes()),
        CTParagraphStyleSpecifier::LineSpacingAdjustment => write(&s.line_spacing_adjustment.to_ne_bytes()),
        CTParagraphStyleSpecifier::LineBoundsOptions => write(&s.line_bounds_options.to_ne_bytes()),
        _ => false,
    }
}

// Text tabs.

pub(crate) struct TabIvars {
    alignment: u8,
    location: f64,
    options: Option<Retained<NSDictionary<NSString, AnyObject>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; tabs are immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTTextTab"]
    #[ivars = TabIvars]
    pub(crate) struct CTTextTabImpl;

    unsafe impl NSObjectProtocol for CTTextTabImpl {}
);

fn tab_imp(t: &CTTextTab) -> &CTTextTabImpl {
    // SAFETY: every CTTextTab is a CTTextTabImpl.
    unsafe { &*(t as *const CTTextTab).cast::<CTTextTabImpl>() }
}

fn new_tab(
    alignment: u8,
    location: f64,
    options: Option<Retained<NSDictionary<NSString, AnyObject>>>,
) -> Retained<CTTextTabImpl> {
    let this = CTTextTabImpl::alloc().set_ivars(TabIvars { alignment, location, options });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTextTabGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_TEXT_TAB
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTextTabCreate(
    alignment: CTTextAlignment,
    location: f64,
    options: Option<&CFDictionary>,
) -> Option<NonNull<CTTextTab>> {
    // SAFETY: a CFDictionary is an NSDictionary here.
    let options =
        options.map(|o| unsafe { &*(o as *const CFDictionary).cast::<NSDictionary<NSString, AnyObject>>() }.copy());
    Some(owned(new_tab(alignment.0, location, options)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTextTabGetAlignment(tab: Option<&CTTextTab>) -> CTTextAlignment {
    CTTextAlignment(tab.map_or(0, |t| tab_imp(t).ivars().alignment))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTextTabGetLocation(tab: Option<&CTTextTab>) -> f64 {
    tab.map_or(0.0, |t| tab_imp(t).ivars().location)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTextTabGetOptions(tab: Option<&CTTextTab>) -> Option<NonNull<CFDictionary>> {
    tab_imp(tab?).ivars().options.as_deref().map(super::borrowed)
}

// Glyph infos.

pub(crate) struct GlyphInfoIvars {
    glyph: CGGlyph,
    name: Option<Retained<NSString>>,
    cid: CGFontIndex,
    collection: CTCharacterCollection,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; infos are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTGlyphInfo"]
    #[ivars = GlyphInfoIvars]
    pub(crate) struct CTGlyphInfoImpl;

    unsafe impl NSObjectProtocol for CTGlyphInfoImpl {}
);

fn info_imp(i: &CTGlyphInfo) -> &CTGlyphInfoImpl {
    // SAFETY: every CTGlyphInfo is a CTGlyphInfoImpl.
    unsafe { &*(i as *const CTGlyphInfo).cast::<CTGlyphInfoImpl>() }
}

fn new_info(ivars: GlyphInfoIvars) -> NonNull<CTGlyphInfo> {
    let this = CTGlyphInfoImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    let made: Retained<CTGlyphInfoImpl> = unsafe { msg_send![super(this), init] };
    owned(made)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTGlyphInfoGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_GLYPH_INFO
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTGlyphInfoCreateWithGlyphName(
    name: Option<&CFString>,
    font: Option<&CTFont>,
    _base: Option<&CFString>,
) -> Option<NonNull<CTGlyphInfo>> {
    let (name, font) = (name?, font?);
    let glyph = super::font::CTFontGetGlyphWithName(Some(font), Some(name));
    let name = Some(NSString::from_str(&text_of(name)));
    Some(new_info(GlyphInfoIvars { glyph, name, cid: 0, collection: CTCharacterCollection::IdentityMapping }))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTGlyphInfoCreateWithGlyph(
    glyph: CGGlyph,
    font: Option<&CTFont>,
    _base: Option<&CFString>,
) -> Option<NonNull<CTGlyphInfo>> {
    let font = font?;
    let name = super::font::CTFontCopyNameForGlyph(Some(font), glyph)
        // SAFETY: a +1 string from the copy.
        .map(|n| unsafe { Retained::from_raw(n.as_ptr().cast::<NSString>()) }.expect("a string"));
    Some(new_info(GlyphInfoIvars { glyph, name, cid: 0, collection: CTCharacterCollection::IdentityMapping }))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTGlyphInfoCreateWithCharacterIdentifier(
    cid: CGFontIndex,
    collection: CTCharacterCollection,
    _base: Option<&CFString>,
) -> Option<NonNull<CTGlyphInfo>> {
    Some(new_info(GlyphInfoIvars { glyph: 0, name: None, cid, collection }))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTGlyphInfoGetGlyphName(info: Option<&CTGlyphInfo>) -> Option<NonNull<CFString>> {
    info_imp(info?).ivars().name.as_deref().map(super::borrowed)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTGlyphInfoGetGlyph(info: Option<&CTGlyphInfo>) -> CGGlyph {
    info.map_or(0, |i| info_imp(i).ivars().glyph)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTGlyphInfoGetCharacterIdentifier(info: Option<&CTGlyphInfo>) -> CGFontIndex {
    info.map_or(0, |i| info_imp(i).ivars().cid)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTGlyphInfoGetCharacterCollection(info: Option<&CTGlyphInfo>) -> CTCharacterCollection {
    info.map_or(CTCharacterCollection::IdentityMapping, |i| info_imp(i).ivars().collection)
}

// Run delegates.

pub(crate) struct DelegateIvars {
    callbacks: CTRunDelegateCallbacks,
    ref_con: *mut c_void,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the delegate calls
    // its dealloc callback once, when it goes.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTRunDelegate"]
    #[ivars = DelegateIvars]
    pub(crate) struct CTRunDelegateImpl;

    unsafe impl NSObjectProtocol for CTRunDelegateImpl {}
);

impl Drop for CTRunDelegateImpl {
    fn drop(&mut self) {
        let i = self.ivars();
        if let Some(dealloc) = i.callbacks.dealloc {
            // The program's pointer as it gave it, null included (the type
            // says non-null; the ABI is the same).
            // SAFETY: the two function types differ only in the pointer's
            // non-null promise.
            let dealloc: unsafe extern "C-unwind" fn(*mut c_void) = unsafe { std::mem::transmute(dealloc) };
            // SAFETY: the program's callback, with its pointer.
            unsafe { dealloc(i.ref_con) };
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunDelegateGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_RUN_DELEGATE
}

/// The delegate is kept, and its dealloc callback called when it goes;
/// layout doesn't ask it for sizes.
///
/// # Safety
///
/// `callbacks` points at the callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTRunDelegateCreate(
    callbacks: *const CTRunDelegateCallbacks,
    ref_con: *mut c_void,
) -> Option<NonNull<CTRunDelegate>> {
    // SAFETY: as the caller promises.
    let callbacks = unsafe { callbacks.as_ref()? };
    let copy = CTRunDelegateCallbacks {
        version: callbacks.version,
        dealloc: callbacks.dealloc,
        getAscent: callbacks.getAscent,
        getDescent: callbacks.getDescent,
        getWidth: callbacks.getWidth,
    };
    let this = CTRunDelegateImpl::alloc().set_ivars(DelegateIvars { callbacks: copy, ref_con });
    // SAFETY: NSObject's designated initializer.
    let made: Retained<CTRunDelegateImpl> = unsafe { msg_send![super(this), init] };
    Some(owned(made))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunDelegateGetRefCon(delegate: Option<&CTRunDelegate>) -> Option<NonNull<c_void>> {
    // SAFETY: every CTRunDelegate is a CTRunDelegateImpl.
    let imp = unsafe { &*(delegate? as *const CTRunDelegate).cast::<CTRunDelegateImpl>() };
    NonNull::new(imp.ivars().ref_con)
}

// Ruby annotations.

pub(crate) struct RubyIvars {
    alignment: CTRubyAlignment,
    overhang: CTRubyOverhang,
    size_factor: f64,
    texts: [Option<Retained<NSString>>; 4],
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; annotations are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTRubyAnnotation"]
    #[ivars = RubyIvars]
    pub(crate) struct CTRubyAnnotationImpl;

    unsafe impl NSObjectProtocol for CTRubyAnnotationImpl {}
);

fn ruby_imp(r: &CTRubyAnnotation) -> &CTRubyAnnotationImpl {
    // SAFETY: every CTRubyAnnotation is a CTRubyAnnotationImpl.
    unsafe { &*(r as *const CTRubyAnnotation).cast::<CTRubyAnnotationImpl>() }
}

fn new_ruby(ivars: RubyIvars) -> NonNull<CTRubyAnnotation> {
    let this = CTRubyAnnotationImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    let made: Retained<CTRubyAnnotationImpl> = unsafe { msg_send![super(this), init] };
    owned(made)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRubyAnnotationGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_RUBY_ANNOTATION
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRubyAnnotationCreateWithAttributes(
    alignment: CTRubyAlignment,
    overhang: CTRubyOverhang,
    position: CTRubyPosition,
    string: Option<&CFString>,
    attributes: Option<&CFDictionary>,
) -> Option<NonNull<CTRubyAnnotation>> {
    let mut texts: [Option<Retained<NSString>>; 4] = Default::default();
    if let Some(slot) = texts.get_mut(position.0 as usize) {
        *slot = string.map(|s| NSString::from_str(&text_of(s)));
    }
    let size_factor = attributes
        .and_then(|a| {
            // SAFETY: a CFDictionary is an NSDictionary here.
            let a = unsafe { &*(a as *const CFDictionary).cast::<NSDictionary<NSString, AnyObject>>() };
            // SAFETY: the constant is this crate's own.
            a.objectForKey(super::ns_string(unsafe { objc2_core_text::kCTRubyAnnotationSizeFactorAttributeName }))
        })
        .and_then(|v| crate::font::number(&v))
        .unwrap_or(0.5);
    Some(new_ruby(RubyIvars { alignment, overhang, size_factor, texts }))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRubyAnnotationCreateCopy(
    ruby: Option<&CTRubyAnnotation>,
) -> Option<NonNull<CTRubyAnnotation>> {
    let i = ruby_imp(ruby?).ivars();
    Some(new_ruby(RubyIvars {
        alignment: i.alignment,
        overhang: i.overhang,
        size_factor: i.size_factor,
        texts: i.texts.clone(),
    }))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRubyAnnotationGetAlignment(ruby: Option<&CTRubyAnnotation>) -> CTRubyAlignment {
    ruby.map_or(CTRubyAlignment::Auto, |r| ruby_imp(r).ivars().alignment)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRubyAnnotationGetOverhang(ruby: Option<&CTRubyAnnotation>) -> CTRubyOverhang {
    ruby.map_or(CTRubyOverhang::Auto, |r| ruby_imp(r).ivars().overhang)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRubyAnnotationGetSizeFactor(ruby: Option<&CTRubyAnnotation>) -> CGFloat {
    ruby.map_or(0.0, |r| ruby_imp(r).ivars().size_factor)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRubyAnnotationGetTextForPosition(
    ruby: Option<&CTRubyAnnotation>,
    position: CTRubyPosition,
) -> Option<NonNull<CFString>> {
    ruby_imp(ruby?).ivars().texts.get(position.0 as usize)?.as_deref().map(super::borrowed)
}
