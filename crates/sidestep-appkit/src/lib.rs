//! AppKit for Sidestep: the classes objc2-app-kit refers to, written in Rust
//! and linked in under their AppKit names.
//!
//! Two threads share the work. The main thread runs AppKit as programs
//! expect: events, timers, the responder chain, views and `drawRect:`.
//! Drawing doesn't touch pixels; it records operations for the parts of a
//! window that changed. A render thread owns the Wayland connection,
//! rasterizes those operations into per-layer caches and presents them
//! (see `backend`). Scroll views get a layer of their own, cut into tiles
//! that the compositor moves when the view scrolls.
//!
//! As in `sidestep-foundation`, each class is a static shell whose loader
//! forces the `define_class!` type, and code here reaches other classes
//! through their objc2-app-kit types.
#![cfg(not(target_vendor = "apple"))]
// `define_class!` recurses once per method, and NSWindow has many.
#![recursion_limit = "256"]

use objc2::ClassType;

/// Load the static shell of the class `T` names. Framework code that makes
/// instances with a `define_class!` type's `alloc()` calls this first,
/// unless it runs in that class's own methods: defining a class before the
/// runtime asks for it fails (docs/architecture.md, "Classes are static
/// shells").
pub(crate) fn load_shell<T: ClassType>() {
    // SAFETY: +class takes nothing and returns the receiver.
    let _: &objc2::runtime::AnyClass = unsafe { objc2::msg_send![T::class(), class] };
}

mod app;
mod backend;
mod clipboard;
mod controls;
mod cursor;
mod desktop;
mod event;
mod font;
mod graphics;
mod inputcontext;
mod keybindings;
mod keycodes;
mod momentum;
mod paragraph;
mod pasteboard;
mod protocol;
mod raster;
mod string_drawing;
#[cfg(test)]
mod test_objects;
mod text;
mod theme;
mod tracking;
mod views;
mod window;

sidestep_runtime::static_class!(pub NSRESPONDER, NSRESPONDER_META = "NSResponder", || {
    let _ = views::NSResponderImpl::class();
});

sidestep_runtime::static_class!(pub NSVIEW, NSVIEW_META = "NSView", || {
    let _ = views::NSViewImpl::class();
});

sidestep_runtime::static_class!(pub NSCLIPVIEW, NSCLIPVIEW_META = "NSClipView", || {
    let _ = views::NSClipViewImpl::class();
});

sidestep_runtime::static_class!(pub NSSCROLLVIEW, NSSCROLLVIEW_META = "NSScrollView", || {
    let _ = views::NSScrollViewImpl::class();
});

sidestep_runtime::static_class!(pub NSWINDOW, NSWINDOW_META = "NSWindow", || {
    let _ = window::NSWindowImpl::class();
});

sidestep_runtime::static_class!(pub NSAPPLICATION, NSAPPLICATION_META = "NSApplication", || {
    let _ = app::NSApplicationImpl::class();
    // Programs make fonts soon after; open the system's in the meantime.
    text::fonts::prewarm();
});

sidestep_runtime::static_class!(pub NSEVENT, NSEVENT_META = "NSEvent", || {
    let _ = event::NSEventImpl::class();
});

sidestep_runtime::static_class!(pub NSCOLOR, NSCOLOR_META = "NSColor", || {
    let _ = graphics::NSColorImpl::class();
    string_drawing::install_string_drawing();
});

sidestep_runtime::static_class!(pub NSFONT, NSFONT_META = "NSFont", || {
    let _ = font::NSFontImpl::class();
    string_drawing::install_string_drawing();
});

sidestep_runtime::static_class!(pub NSFONTDESCRIPTOR, NSFONTDESCRIPTOR_META = "NSFontDescriptor", || {
    let _ = font::NSFontDescriptorImpl::class();
});

sidestep_runtime::static_class!(pub NSPARAGRAPHSTYLE, NSPARAGRAPHSTYLE_META = "NSParagraphStyle", || {
    let _ = paragraph::NSParagraphStyleImpl::class();
    string_drawing::install_string_drawing();
});

sidestep_runtime::static_class!(pub NSMUTABLEPARAGRAPHSTYLE, NSMUTABLEPARAGRAPHSTYLE_META = "NSMutableParagraphStyle", || {
    let _ = paragraph::NSMutableParagraphStyleImpl::class();
});

sidestep_runtime::static_class!(pub NSTEXTTAB, NSTEXTTAB_META = "NSTextTab", || {
    let _ = paragraph::NSTextTabImpl::class();
});

sidestep_runtime::static_class!(pub NSBEZIERPATH, NSBEZIERPATH_META = "NSBezierPath", || {
    let _ = graphics::NSBezierPathImpl::class();
});

sidestep_runtime::static_class!(pub NSPASTEBOARD, NSPASTEBOARD_META = "NSPasteboard", || {
    let _ = pasteboard::NSPasteboardImpl::class();
});

sidestep_runtime::static_class!(pub NSCURSOR, NSCURSOR_META = "NSCursor", || {
    let _ = cursor::NSCursorImpl::class();
});

sidestep_runtime::static_class!(pub NSTRACKINGAREA, NSTRACKINGAREA_META = "NSTrackingArea", || {
    let _ = tracking::NSTrackingAreaImpl::class();
});

sidestep_runtime::static_class!(pub NSTEXTINPUTCONTEXT, NSTEXTINPUTCONTEXT_META = "NSTextInputContext", || {
    let _ = inputcontext::NSTextInputContextImpl::class();
});

// Run loop modes AppKit adds.
sidestep_foundation::constant_string!(NSEventTrackingRunLoopMode = "NSEventTrackingRunLoopMode");
sidestep_foundation::constant_string!(NSModalPanelRunLoopMode = "NSModalPanelRunLoopMode");

// Pasteboard types and names.
sidestep_foundation::constant_string!(NSPasteboardTypeString = "public.utf8-plain-text");
sidestep_foundation::constant_string!(NSPasteboardTypePDF = "com.adobe.pdf");
sidestep_foundation::constant_string!(NSPasteboardTypeTIFF = "public.tiff");
sidestep_foundation::constant_string!(NSPasteboardTypePNG = "public.png");
sidestep_foundation::constant_string!(NSPasteboardTypeRTF = "public.rtf");
sidestep_foundation::constant_string!(NSPasteboardTypeRTFD = "com.apple.flat-rtfd");
sidestep_foundation::constant_string!(NSPasteboardTypeHTML = "public.html");
sidestep_foundation::constant_string!(NSPasteboardTypeTabularText = "public.utf8-tab-separated-values-text");
sidestep_foundation::constant_string!(NSPasteboardTypeURL = "public.url");
sidestep_foundation::constant_string!(NSPasteboardTypeFileURL = "public.file-url");
sidestep_foundation::constant_string!(NSPasteboardNameGeneral = "Apple CFPasteboard general");
sidestep_foundation::constant_string!(NSPasteboardNameFind = "Apple CFPasteboard find");
sidestep_foundation::constant_string!(NSPasteboardTypeFont = "com.apple.cocoa.pasteboard.character-formatting");
sidestep_foundation::constant_string!(NSPasteboardTypeRuler = "com.apple.cocoa.pasteboard.paragraph-formatting");
sidestep_foundation::constant_string!(NSPasteboardTypeColor = "com.apple.cocoa.pasteboard.color");
sidestep_foundation::constant_string!(NSPasteboardTypeSound = "com.apple.cocoa.pasteboard.sound");
sidestep_foundation::constant_string!(
    NSPasteboardTypeMultipleTextSelection = "com.apple.cocoa.pasteboard.multiple-text-selection"
);
sidestep_foundation::constant_string!(
    NSPasteboardTypeTextFinderOptions = "com.apple.cocoa.pasteboard.find-panel-search-options"
);
// The old names, which the pasteboard takes as the types above.
sidestep_foundation::constant_string!(NSStringPboardType = "NSStringPboardType");
sidestep_foundation::constant_string!(NSFilenamesPboardType = "NSFilenamesPboardType");
sidestep_foundation::constant_string!(NSTIFFPboardType = "NeXT TIFF v4.0 pasteboard type");
sidestep_foundation::constant_string!(NSRTFPboardType = "NeXT Rich Text Format v1.0 pasteboard type");
sidestep_foundation::constant_string!(NSRTFDPboardType = "NeXT RTFD pasteboard type");
sidestep_foundation::constant_string!(NSTabularTextPboardType = "NeXT tabular text pasteboard type");
sidestep_foundation::constant_string!(NSFontPboardType = "NeXT font pasteboard type");
sidestep_foundation::constant_string!(NSRulerPboardType = "NeXT ruler pasteboard type");
sidestep_foundation::constant_string!(NSColorPboardType = "NSColor pasteboard type");
sidestep_foundation::constant_string!(NSHTMLPboardType = "Apple HTML pasteboard type");
sidestep_foundation::constant_string!(NSURLPboardType = "Apple URL pasteboard type");
sidestep_foundation::constant_string!(NSPDFPboardType = "Apple PDF pasteboard type");
sidestep_foundation::constant_string!(
    NSMultipleTextSelectionPboardType = "Apple multiple text selection pasteboard type"
);
sidestep_foundation::constant_string!(NSPostScriptPboardType = "NeXT Encapsulated PostScript v1.2 pasteboard type");
sidestep_foundation::constant_string!(NSVCardPboardType = "Apple VCard pasteboard type");
sidestep_foundation::constant_string!(NSInkTextPboardType = "Apple InkText pasteboard type");
sidestep_foundation::constant_string!(NSFilesPromisePboardType = "Apple files promise pasteboard type");

// A helper whose loader gives NSString its drawing methods (see
// `string_drawing`).
sidestep_runtime::static_class!(
    pub(crate) STRING_DRAWING,
    STRING_DRAWING_META = "_SidestepStringDrawing",
    string_drawing::load
);

// Attribute names for attributed strings and string drawing, with the
// values macOS gives them (conformance/tests/text.rs compares).
sidestep_foundation::constant_string!(NSFontAttributeName = "NSFont");
sidestep_foundation::constant_string!(NSParagraphStyleAttributeName = "NSParagraphStyle");
sidestep_foundation::constant_string!(NSForegroundColorAttributeName = "NSColor");
sidestep_foundation::constant_string!(NSBackgroundColorAttributeName = "NSBackgroundColor");
sidestep_foundation::constant_string!(NSLigatureAttributeName = "NSLigature");
sidestep_foundation::constant_string!(NSKernAttributeName = "NSKern");
sidestep_foundation::constant_string!(NSTrackingAttributeName = "CTTracking");
sidestep_foundation::constant_string!(NSStrikethroughStyleAttributeName = "NSStrikethrough");
sidestep_foundation::constant_string!(NSUnderlineStyleAttributeName = "NSUnderline");
sidestep_foundation::constant_string!(NSStrokeColorAttributeName = "NSStrokeColor");
sidestep_foundation::constant_string!(NSStrokeWidthAttributeName = "NSStrokeWidth");
sidestep_foundation::constant_string!(NSShadowAttributeName = "NSShadow");
sidestep_foundation::constant_string!(NSTextEffectAttributeName = "NSTextEffect");
sidestep_foundation::constant_string!(NSAttachmentAttributeName = "NSAttachment");
sidestep_foundation::constant_string!(NSLinkAttributeName = "NSLink");
sidestep_foundation::constant_string!(NSBaselineOffsetAttributeName = "NSBaselineOffset");
sidestep_foundation::constant_string!(NSUnderlineColorAttributeName = "NSUnderlineColor");
sidestep_foundation::constant_string!(NSStrikethroughColorAttributeName = "NSStrikethroughColor");
sidestep_foundation::constant_string!(NSObliquenessAttributeName = "NSObliqueness");
sidestep_foundation::constant_string!(NSExpansionAttributeName = "NSExpansion");
sidestep_foundation::constant_string!(NSWritingDirectionAttributeName = "NSWritingDirection");
sidestep_foundation::constant_string!(NSVerticalGlyphFormAttributeName = "CTVerticalForms");
sidestep_foundation::constant_string!(NSCursorAttributeName = "NSCursor");
sidestep_foundation::constant_string!(NSToolTipAttributeName = "NSToolTip");
sidestep_foundation::constant_string!(NSMarkedClauseSegmentAttributeName = "NSMarkedClauseSegment");
sidestep_foundation::constant_string!(NSTextAlternativesAttributeName = "NSTextAlternatives");
sidestep_foundation::constant_string!(NSSpellingStateAttributeName = "NSSpellingState");
sidestep_foundation::constant_string!(NSSuperscriptAttributeName = "NSSuperScript");
sidestep_foundation::constant_string!(NSGlyphInfoAttributeName = "NSGlyphInfo");
sidestep_foundation::constant_string!(NSTabColumnTerminatorsAttributeName = "NSTabColumnTerminatorsAttributeName");

// Font descriptor attributes, traits, designs and text styles.
sidestep_foundation::constant_string!(NSFontFamilyAttribute = "NSFontFamilyAttribute");
sidestep_foundation::constant_string!(NSFontNameAttribute = "NSFontNameAttribute");
sidestep_foundation::constant_string!(NSFontFaceAttribute = "NSFontFaceAttribute");
sidestep_foundation::constant_string!(NSFontSizeAttribute = "NSFontSizeAttribute");
sidestep_foundation::constant_string!(NSFontVisibleNameAttribute = "NSFontVisibleNameAttribute");
sidestep_foundation::constant_string!(NSFontMatrixAttribute = "NSFontMatrixAttribute");
sidestep_foundation::constant_string!(NSFontVariationAttribute = "NSCTFontVariationAttribute");
sidestep_foundation::constant_string!(NSFontCharacterSetAttribute = "NSCTFontCharacterSetAttribute");
sidestep_foundation::constant_string!(NSFontCascadeListAttribute = "NSCTFontCascadeListAttribute");
sidestep_foundation::constant_string!(NSFontTraitsAttribute = "NSCTFontTraitsAttribute");
sidestep_foundation::constant_string!(NSFontFixedAdvanceAttribute = "NSCTFontFixedAdvanceAttribute");
sidestep_foundation::constant_string!(NSFontFeatureSettingsAttribute = "NSCTFontFeatureSettingsAttribute");
sidestep_foundation::constant_string!(NSFontFeatureTypeIdentifierKey = "CTFeatureTypeIdentifier");
sidestep_foundation::constant_string!(NSFontFeatureSelectorIdentifierKey = "CTFeatureSelectorIdentifier");
sidestep_foundation::constant_string!(NSFontVariationAxisIdentifierKey = "NSCTVariationAxisIdentifier");
sidestep_foundation::constant_string!(NSFontVariationAxisMinimumValueKey = "NSCTVariationAxisMinimumValue");
sidestep_foundation::constant_string!(NSFontVariationAxisMaximumValueKey = "NSCTVariationAxisMaximumValue");
sidestep_foundation::constant_string!(NSFontVariationAxisDefaultValueKey = "NSCTVariationAxisDefaultValue");
sidestep_foundation::constant_string!(NSFontVariationAxisNameKey = "NSCTVariationAxisName");
sidestep_foundation::constant_string!(NSFontSymbolicTrait = "NSCTFontSymbolicTrait");
sidestep_foundation::constant_string!(NSFontWeightTrait = "NSCTFontWeightTrait");
sidestep_foundation::constant_string!(NSFontWidthTrait = "NSCTFontProportionTrait");
sidestep_foundation::constant_string!(NSFontSlantTrait = "NSCTFontSlantTrait");
sidestep_foundation::constant_string!(NSFontDescriptorSystemDesignDefault = "NSCTFontUIFontDesignDefault");
sidestep_foundation::constant_string!(NSFontDescriptorSystemDesignSerif = "NSCTFontUIFontDesignSerif");
sidestep_foundation::constant_string!(NSFontDescriptorSystemDesignMonospaced = "NSCTFontUIFontDesignMonospaced");
sidestep_foundation::constant_string!(NSFontDescriptorSystemDesignRounded = "NSCTFontUIFontDesignRounded");
sidestep_foundation::constant_string!(NSFontTextStyleLargeTitle = "UICTFontTextStyleTitle0");
sidestep_foundation::constant_string!(NSFontTextStyleTitle1 = "UICTFontTextStyleTitle1");
sidestep_foundation::constant_string!(NSFontTextStyleTitle2 = "UICTFontTextStyleTitle2");
sidestep_foundation::constant_string!(NSFontTextStyleTitle3 = "UICTFontTextStyleTitle3");
sidestep_foundation::constant_string!(NSFontTextStyleHeadline = "UICTFontTextStyleHeadline");
sidestep_foundation::constant_string!(NSFontTextStyleSubheadline = "UICTFontTextStyleSubhead");
sidestep_foundation::constant_string!(NSFontTextStyleBody = "UICTFontTextStyleBody");
sidestep_foundation::constant_string!(NSFontTextStyleCallout = "UICTFontTextStyleCallout");
sidestep_foundation::constant_string!(NSFontTextStyleFootnote = "UICTFontTextStyleFootnote");
sidestep_foundation::constant_string!(NSFontTextStyleCaption1 = "UICTFontTextStyleCaption1");
sidestep_foundation::constant_string!(NSFontTextStyleCaption2 = "UICTFontTextStyleCaption2");

// Font widths, as `NSFontWidth` values (single precision on macOS too).
#[unsafe(no_mangle)]
pub static NSFontWidthCompressed: f64 = -0.3f32 as f64;
#[unsafe(no_mangle)]
pub static NSFontWidthCondensed: f64 = -0.2f32 as f64;
#[unsafe(no_mangle)]
pub static NSFontWidthStandard: f64 = 0.0;
#[unsafe(no_mangle)]
pub static NSFontWidthExpanded: f64 = 0.2f32 as f64;

// Keys only newer macOS versions define (values as macOS prints them).
sidestep_foundation::constant_string!(NSCharacterShapeAttributeName = "NSCharacterShape");
sidestep_foundation::constant_string!(NSTextHighlightStyleAttributeName = "NSTextHighlightStyle");
sidestep_foundation::constant_string!(NSTextHighlightColorSchemeAttributeName = "NSTextHighlightColorScheme");
sidestep_foundation::constant_string!(NSAdaptiveImageGlyphAttributeName = "CTAdaptiveImageProvider");
sidestep_foundation::constant_string!(NSWritingToolsExclusionAttributeName = "WTWritingToolsPreserved");

// Font weights, as `NSFontWeight` values (single precision on macOS too).
#[unsafe(no_mangle)]
pub static NSFontWeightUltraLight: f64 = -0.8f32 as f64;
#[unsafe(no_mangle)]
pub static NSFontWeightThin: f64 = -0.6f32 as f64;
#[unsafe(no_mangle)]
pub static NSFontWeightLight: f64 = -0.4f32 as f64;
#[unsafe(no_mangle)]
pub static NSFontWeightRegular: f64 = 0.0;
#[unsafe(no_mangle)]
pub static NSFontWeightMedium: f64 = 0.23f32 as f64;
#[unsafe(no_mangle)]
pub static NSFontWeightSemibold: f64 = 0.3f32 as f64;
#[unsafe(no_mangle)]
pub static NSFontWeightBold: f64 = 0.4f32 as f64;
#[unsafe(no_mangle)]
pub static NSFontWeightHeavy: f64 = 0.56f32 as f64;
#[unsafe(no_mangle)]
pub static NSFontWeightBlack: f64 = 0.62f32 as f64;
