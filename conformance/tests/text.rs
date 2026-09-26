//! Fonts, paragraph styles and string drawing, checked on macOS and on
//! Linux alike: what holds whatever fonts a system has. Exact metrics
//! differ between platforms and are never asserted; relations are (a
//! monospaced font is fixed pitch, longer text is wider, wrapping makes
//! text narrower and taller, spacing adds what it says).
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, msg_send};
use objc2_app_kit::{
    NSBackgroundColorAttributeName, NSBaselineOffsetAttributeName, NSControlSize, NSFont, NSFontAttributeName,
    NSFontDescriptor, NSFontDescriptorSymbolicTraits, NSFontDescriptorSystemDesignMonospaced, NSFontFamilyAttribute,
    NSFontFeatureSelectorIdentifierKey, NSFontFeatureSettingsAttribute, NSFontFeatureTypeIdentifierKey,
    NSFontNameAttribute, NSFontTextStyleBody, NSFontTextStyleFootnote, NSFontTextStyleHeadline,
    NSFontTextStyleLargeTitle, NSFontTextStyleTitle1, NSFontTraitsAttribute, NSFontWeightBold, NSFontWeightLight,
    NSFontWeightRegular, NSFontWeightSemibold, NSFontWeightTrait, NSForegroundColorAttributeName, NSKernAttributeName,
    NSLigatureAttributeName, NSLineBreakMode, NSMutableParagraphStyle, NSParagraphStyle, NSParagraphStyleAttributeName,
    NSStrikethroughColorAttributeName, NSStrikethroughStyleAttributeName, NSStringDrawing, NSStringDrawingOptions,
    NSStringNSExtendedStringDrawing, NSTextAlignment, NSTextTab, NSTextTabType, NSTrackingAttributeName,
    NSUnderlineColorAttributeName, NSUnderlineStyleAttributeName, NSWritingDirection,
};
use objc2_foundation::{NSDictionary, NSRect, NSSize, NSString, ns_string};

use sidestep as _;

type Attributes = Retained<NSDictionary<NSString, AnyObject>>;

fn attrs(font: &NSFont) -> Attributes {
    // SAFETY: the key is a constant string.
    let key = unsafe { NSFontAttributeName };
    NSDictionary::from_slices(&[key], &[font as &AnyObject])
}

fn attrs_with_style(font: &NSFont, style: &NSParagraphStyle) -> Attributes {
    // SAFETY: the keys are constant strings.
    let keys = unsafe { [NSFontAttributeName, NSParagraphStyleAttributeName] };
    NSDictionary::from_slices(&keys, &[font as &AnyObject, style as &AnyObject])
}

fn size(text: &str, attrs: &Attributes) -> NSSize {
    // SAFETY: the dictionary holds valid attributes.
    unsafe { NSString::from_str(text).sizeWithAttributes(Some(attrs)) }
}

fn bounds(text: &str, width: f64, height: f64, options: NSStringDrawingOptions, attrs: &Attributes) -> NSRect {
    // SAFETY: the dictionary holds valid attributes.
    unsafe {
        NSString::from_str(text).boundingRectWithSize_options_attributes_context(
            NSSize::new(width, height),
            options,
            Some(attrs),
            None,
        )
    }
}

fn lines() -> NSStringDrawingOptions {
    NSStringDrawingOptions::UsesLineFragmentOrigin
}

fn close(a: f64, b: f64, tolerance: f64) -> bool {
    (a - b).abs() <= tolerance
}

fn helvetica(size: f64) -> Retained<NSFont> {
    NSFont::userFontOfSize(size).expect("the user font")
}

fn traits(font: &NSFont) -> NSFontDescriptorSymbolicTraits {
    font.fontDescriptor().symbolicTraits()
}

fn style(set: impl FnOnce(&NSMutableParagraphStyle)) -> Retained<NSMutableParagraphStyle> {
    let style = NSMutableParagraphStyle::new();
    set(&style);
    style
}

const LONG: &str = "The quick brown fox jumps over the lazy dog and keeps on running far away";

fn attribute_names() {
    // SAFETY: the statics are constant strings.
    let names: [(&NSString, &str); 20] = unsafe {
        [
            (NSFontAttributeName, "NSFont"),
            (NSParagraphStyleAttributeName, "NSParagraphStyle"),
            (NSForegroundColorAttributeName, "NSColor"),
            (NSBackgroundColorAttributeName, "NSBackgroundColor"),
            (NSLigatureAttributeName, "NSLigature"),
            (NSKernAttributeName, "NSKern"),
            (NSStrikethroughStyleAttributeName, "NSStrikethrough"),
            (NSUnderlineStyleAttributeName, "NSUnderline"),
            (NSBaselineOffsetAttributeName, "NSBaselineOffset"),
            (NSUnderlineColorAttributeName, "NSUnderlineColor"),
            (NSStrikethroughColorAttributeName, "NSStrikethroughColor"),
            (NSFontDescriptorSystemDesignMonospaced, "NSCTFontUIFontDesignMonospaced"),
            (NSTrackingAttributeName, "CTTracking"),
            (NSFontFamilyAttribute, "NSFontFamilyAttribute"),
            (NSFontNameAttribute, "NSFontNameAttribute"),
            (NSFontTraitsAttribute, "NSCTFontTraitsAttribute"),
            (NSFontWeightTrait, "NSCTFontWeightTrait"),
            (NSFontFeatureSettingsAttribute, "NSCTFontFeatureSettingsAttribute"),
            (NSFontFeatureTypeIdentifierKey, "CTFeatureTypeIdentifier"),
            (NSFontFeatureSelectorIdentifierKey, "CTFeatureSelectorIdentifier"),
        ]
    };
    for (name, value) in names {
        assert_eq!(name.to_string(), value);
    }
}

fn font_sizes() {
    assert_eq!(NSFont::systemFontOfSize(13.0).pointSize(), 13.0);
    assert_eq!(NSFont::systemFontOfSize(0.0).pointSize(), NSFont::systemFontSize());
    assert_eq!(NSFont::systemFontSize(), 13.0);
    assert_eq!(NSFont::smallSystemFontSize(), 11.0);
    assert_eq!(NSFont::labelFontSize(), 10.0);
    assert_eq!(NSFont::labelFontOfSize(0.0).pointSize(), NSFont::labelFontSize());
    assert_eq!(NSFont::boldSystemFontOfSize(21.0).pointSize(), 21.0);
    assert_eq!(NSFont::systemFontOfSize(13.0).fontWithSize(20.0).pointSize(), 20.0);
    assert!(NSFont::userFontOfSize(0.0).unwrap().pointSize() > 0.0);
    assert!(NSFont::userFixedPitchFontOfSize(-1.0).unwrap().pointSize() > 0.0);
    // SAFETY: the weights are valid NSFontWeight values.
    unsafe {
        assert_eq!(NSFont::monospacedSystemFontOfSize_weight(15.0, NSFontWeightRegular).pointSize(), 15.0);
        assert_eq!(NSFont::monospacedDigitSystemFontOfSize_weight(16.0, NSFontWeightRegular).pointSize(), 16.0);
    }
}

fn text_styles() {
    let empty = NSDictionary::new();
    // SAFETY: the styles are constant strings; the options are empty.
    let font = |style: &NSString| unsafe { NSFont::preferredFontForTextStyle_options(style, &empty) };
    let (large, title, body, headline, footnote) = unsafe {
        (
            font(NSFontTextStyleLargeTitle),
            font(NSFontTextStyleTitle1),
            font(NSFontTextStyleBody),
            font(NSFontTextStyleHeadline),
            font(NSFontTextStyleFootnote),
        )
    };
    assert_eq!(body.pointSize(), NSFont::systemFontSize());
    assert!(large.pointSize() > title.pointSize() && title.pointSize() > body.pointSize());
    assert!(footnote.pointSize() < body.pointSize());
    assert!(traits(&headline).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    assert!(!traits(&body).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    // SAFETY: as above.
    let descriptor =
        unsafe { NSFontDescriptor::preferredFontDescriptorForTextStyle_options(NSFontTextStyleBody, &empty) };
    assert_eq!(descriptor.pointSize(), body.pointSize());
    assert_eq!(NSFont::systemFontSizeForControlSize(NSControlSize::Regular), 13.0);
    assert_eq!(NSFont::systemFontSizeForControlSize(NSControlSize::Small), 11.0);
    assert_eq!(NSFont::systemFontSizeForControlSize(NSControlSize::Mini), 9.0);
}

fn font_weights_and_traits() {
    let regular = NSFont::systemFontOfSize(13.0);
    assert!(!traits(&regular).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    assert!(!traits(&regular).contains(NSFontDescriptorSymbolicTraits::TraitItalic));
    assert!(traits(&NSFont::boldSystemFontOfSize(13.0)).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    // SAFETY: the weights are valid NSFontWeight values.
    unsafe {
        assert!(
            traits(&NSFont::systemFontOfSize_weight(13.0, NSFontWeightSemibold))
                .contains(NSFontDescriptorSymbolicTraits::TraitBold)
        );
        assert!(
            traits(&NSFont::systemFontOfSize_weight(13.0, NSFontWeightBold))
                .contains(NSFontDescriptorSymbolicTraits::TraitBold)
        );
        assert!(
            !traits(&NSFont::systemFontOfSize_weight(13.0, NSFontWeightLight))
                .contains(NSFontDescriptorSymbolicTraits::TraitBold)
        );
    }

    let descriptor = regular.fontDescriptor();
    let italic = descriptor.fontDescriptorWithSymbolicTraits(NSFontDescriptorSymbolicTraits::TraitItalic);
    let font = NSFont::fontWithDescriptor_size(&italic, 15.0).unwrap();
    assert_eq!(font.pointSize(), 15.0);
    assert!(traits(&font).contains(NSFontDescriptorSymbolicTraits::TraitItalic));
    let both = descriptor.fontDescriptorWithSymbolicTraits(
        NSFontDescriptorSymbolicTraits::TraitItalic | NSFontDescriptorSymbolicTraits::TraitBold,
    );
    let font = NSFont::fontWithDescriptor_size(&both, 0.0).unwrap();
    assert_eq!(font.pointSize(), 13.0, "size 0 keeps the descriptor's");
    assert!(traits(&font).contains(NSFontDescriptorSymbolicTraits::TraitItalic));
    assert!(traits(&font).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    let bold = descriptor.fontDescriptorWithSymbolicTraits(NSFontDescriptorSymbolicTraits::TraitBold);
    assert!(bold.symbolicTraits().contains(NSFontDescriptorSymbolicTraits::TraitBold));
    assert!(
        traits(&NSFont::fontWithDescriptor_size(&bold, 0.0).unwrap())
            .contains(NSFontDescriptorSymbolicTraits::TraitBold)
    );
    assert_eq!(descriptor.fontDescriptorWithSize(30.0).pointSize(), 30.0);
    assert_eq!(descriptor.pointSize(), 13.0);
}

fn fixed_pitch() {
    // SAFETY: the weight is a valid NSFontWeight value.
    let mono = unsafe { NSFont::monospacedSystemFontOfSize_weight(13.0, NSFontWeightRegular) };
    assert!(mono.isFixedPitch());
    assert!(traits(&mono).contains(NSFontDescriptorSymbolicTraits::TraitMonoSpace));
    assert!(!NSFont::systemFontOfSize(13.0).isFixedPitch());
    assert!(NSFont::userFixedPitchFontOfSize(0.0).unwrap().isFixedPitch());

    let descriptor = NSFont::systemFontOfSize(13.0).fontDescriptor();
    let asked = descriptor.fontDescriptorWithSymbolicTraits(NSFontDescriptorSymbolicTraits::TraitMonoSpace);
    assert!(NSFont::fontWithDescriptor_size(&asked, 0.0).unwrap().isFixedPitch());
    // SAFETY: the design is a constant string.
    let design = unsafe { descriptor.fontDescriptorWithDesign(NSFontDescriptorSystemDesignMonospaced) };
    let font = NSFont::fontWithDescriptor_size(&design.expect("a monospaced design"), 0.0).unwrap();
    assert!(font.isFixedPitch());

    // Every character of a fixed-pitch font is as wide as every other.
    let a = attrs(&mono);
    assert!(close(size("iiii", &a).width, size("WWWW", &a).width, 0.01));
    let proportional = attrs(&NSFont::systemFontOfSize(13.0));
    assert!(size("iiii", &proportional).width < size("WWWW", &proportional).width);
}

fn metrics() {
    for font in [
        NSFont::systemFontOfSize(13.0),
        NSFont::boldSystemFontOfSize(13.0),
        NSFont::userFixedPitchFontOfSize(13.0).unwrap(),
        helvetica(13.0),
    ] {
        assert!(font.ascender() > 0.0 && font.ascender() < 2.0 * 13.0);
        assert!(font.descender() < 0.0 && font.descender() > -13.0);
        assert!(font.leading() >= 0.0);
        assert!(font.capHeight() > font.xHeight() && font.xHeight() > 0.0);
        assert!(font.capHeight() <= font.ascender());
        assert!(font.underlinePosition() < 0.0);
        assert!(font.underlineThickness() > 0.0);
        let bbox = font.boundingRectForFont();
        assert!(bbox.size.height > 0.0 && bbox.size.width > 0.0);
        assert!(font.maximumAdvancement().width > 0.0);

        // Metrics grow with the size.
        let double = font.fontWithSize(26.0);
        assert!(close(double.ascender(), 2.0 * font.ascender(), 0.5));
        assert!(close(double.descender(), 2.0 * font.descender(), 0.5));
    }
}

fn names() {
    assert!(NSFont::fontWithName_size(ns_string!("NoSuchFontAnywhere Xyzzy"), 12.0).is_none());
    for font in [helvetica(12.0), NSFont::userFixedPitchFontOfSize(12.0).unwrap()] {
        let family = font.familyName().expect("a family");
        assert!(!family.to_string().is_empty());
        assert!(!font.fontName().to_string().is_empty());
        assert!(!font.displayName().expect("a display name").to_string().is_empty());
        // A font can be found again by its PostScript name and its family.
        let again = NSFont::fontWithName_size(&font.fontName(), 17.0).expect("found by name");
        assert_eq!(again.pointSize(), 17.0);
        assert_eq!(again.familyName(), Some(family.clone()));
        assert_eq!(again.fontName(), font.fontName());
        let by_family = NSFont::fontWithName_size(&family, 0.0).expect("found by family");
        assert_eq!(by_family.familyName(), Some(family));
        assert_eq!(by_family.pointSize(), 12.0, "size 0 is 12 points");
        assert_eq!(font.isFixedPitch(), again.isFixedPitch());
    }
}

fn font_equality() {
    let a = NSFont::systemFontOfSize(13.0);
    let b = NSFont::systemFontOfSize(13.0);
    assert!(a.isEqual(Some(&b)));
    assert_eq!(a.hash(), b.hash());
    assert!(!a.isEqual(Some(&NSFont::systemFontOfSize(14.0))));
    assert!(!a.isEqual(Some(&NSFont::boldSystemFontOfSize(13.0))));
    let copy: Retained<NSFont> = unsafe { msg_send![&*a, copy] };
    assert!(copy.isEqual(Some(&a)));
    let descriptor = a.fontDescriptor();
    assert!(descriptor.isEqual(Some(&b.fontDescriptor())));
    assert!(!descriptor.isEqual(Some(&descriptor.fontDescriptorWithSize(30.0))));
}

fn paragraph_defaults() {
    for style in [NSParagraphStyle::defaultParagraphStyle(), Retained::into_super(NSMutableParagraphStyle::new())] {
        assert_eq!(style.alignment(), NSTextAlignment::Natural);
        assert_eq!(style.lineBreakMode(), NSLineBreakMode::ByWordWrapping);
        assert_eq!(style.baseWritingDirection(), NSWritingDirection::Natural);
        assert_eq!(style.lineSpacing(), 0.0);
        assert_eq!(style.paragraphSpacing(), 0.0);
        assert_eq!(style.paragraphSpacingBefore(), 0.0);
        assert_eq!(style.headIndent(), 0.0);
        assert_eq!(style.firstLineHeadIndent(), 0.0);
        assert_eq!(style.tailIndent(), 0.0);
        assert_eq!(style.minimumLineHeight(), 0.0);
        assert_eq!(style.maximumLineHeight(), 0.0);
        assert_eq!(style.lineHeightMultiple(), 0.0);
        assert_eq!(style.defaultTabInterval(), 0.0);
        assert_eq!(style.hyphenationFactor(), 0.0);
    }
    assert!(NSParagraphStyle::defaultParagraphStyle().isEqual(Some(&NSMutableParagraphStyle::new())));
}

fn paragraph_mutation_and_copies() {
    let style = style(|s| {
        s.setAlignment(NSTextAlignment::Center);
        s.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        s.setBaseWritingDirection(NSWritingDirection::RightToLeft);
        s.setLineSpacing(3.0);
        s.setParagraphSpacing(4.0);
        s.setParagraphSpacingBefore(5.0);
        s.setHeadIndent(6.0);
        s.setFirstLineHeadIndent(7.0);
        s.setTailIndent(-8.0);
        s.setMinimumLineHeight(9.0);
        s.setMaximumLineHeight(30.0);
        s.setLineHeightMultiple(1.5);
        s.setDefaultTabInterval(36.0);
    });
    assert_eq!(style.alignment(), NSTextAlignment::Center);
    assert_eq!(style.lineBreakMode(), NSLineBreakMode::ByTruncatingMiddle);
    assert_eq!(style.baseWritingDirection(), NSWritingDirection::RightToLeft);
    assert_eq!(
        [
            style.lineSpacing(),
            style.paragraphSpacing(),
            style.paragraphSpacingBefore(),
            style.headIndent(),
            style.firstLineHeadIndent(),
            style.tailIndent(),
            style.minimumLineHeight(),
            style.maximumLineHeight(),
            style.lineHeightMultiple(),
            style.defaultTabInterval(),
        ],
        [3.0, 4.0, 5.0, 6.0, 7.0, -8.0, 9.0, 30.0, 1.5, 36.0]
    );

    let copy: Retained<NSParagraphStyle> = unsafe { msg_send![&*style, copy] };
    assert!(!copy.isKindOfClass(NSMutableParagraphStyle::class()), "a copy is immutable");
    assert!(copy.isEqual(Some(&style)));
    assert_eq!(copy.lineSpacing(), 3.0);
    let mutable: Retained<NSMutableParagraphStyle> = unsafe { msg_send![&*copy, mutableCopy] };
    assert!(mutable.isKindOfClass(NSMutableParagraphStyle::class()));
    assert!(mutable.isEqual(Some(&style)));
    mutable.setLineSpacing(10.0);
    assert!(!mutable.isEqual(Some(&style)));
    assert_eq!(copy.lineSpacing(), 3.0, "copies don't share storage");

    // (Whether a style set from another equals it is unspecified: on macOS
    // it doesn't, though every value matches.)
    let other = NSMutableParagraphStyle::new();
    other.setParagraphStyle(&style);
    assert_eq!(other.alignment(), NSTextAlignment::Center);
    assert_eq!(other.lineBreakMode(), NSLineBreakMode::ByTruncatingMiddle);
    assert_eq!((other.lineSpacing(), other.tailIndent(), other.lineHeightMultiple()), (3.0, -8.0, 1.5));
}

fn sizes() {
    let a = attrs(&helvetica(12.0));
    let hello = size("Hello", &a);
    assert!(hello.width > 0.0 && hello.height > 0.0);
    let line = hello.height;
    let longer = size("Hello, world", &a);
    assert!(longer.width > hello.width);
    assert_eq!(longer.height, line);
    assert!(size("Hello   ", &a).width > hello.width, "trailing spaces count");
    let empty = size("", &a);
    assert_eq!(empty.width, 0.0);
    assert_eq!(empty.height, line, "an empty string is one line tall");
    assert_eq!(size("Hello\n", &a).height, 2.0 * line);
    let two = size("Hello\nWorld, again", &a);
    assert_eq!(two.height, 2.0 * line);
    assert_eq!(two.width, size("World, again", &a).width, "the widest line");

    let big = size("Hello", &attrs(&helvetica(24.0)));
    assert!(close(big.width, 2.0 * hello.width, 0.1 * hello.width));
    assert!(big.height > line);

    // Without attributes, text is measured in a 12-point font.
    let plain = unsafe { NSString::from_str("Hello").sizeWithAttributes(None) };
    assert!(plain.width > 0.0 && plain.height > 0.0);
}

fn kerning() {
    let a = attrs(&helvetica(24.0));
    let pair = size("AV", &a).width;
    let apart = size("A", &a).width + size("V", &a).width;
    assert!(pair < apart, "AV kerns closer than A and V apart ({pair} vs {apart})");
}

fn bounding_rects() {
    let a = attrs(&helvetica(12.0));
    let line = size("Hello", &a).height;
    let one = bounds("Hello", 0.0, 0.0, lines(), &a);
    assert_eq!(one.origin.x, 0.0);
    assert_eq!(one.origin.y, 0.0);
    assert_eq!(one.size, size("Hello", &a));

    // Without usesLineFragmentOrigin: one line, its origin on the baseline.
    let single = bounds("Hello\nWorld", 0.0, 0.0, NSStringDrawingOptions(0), &a);
    assert_eq!(single.size.height, line);
    assert_eq!(single.size.width, size("Hello", &a).width);
    assert!(single.origin.y < 0.0 && single.origin.y > -line);

    // Wrapping narrows and grows.
    let unwrapped = bounds(LONG, 0.0, 0.0, lines(), &a);
    assert_eq!(unwrapped.size.height, line);
    let wrapped = bounds(LONG, 100.0, 0.0, lines(), &a);
    assert!(wrapped.size.width <= 100.0);
    assert!(wrapped.size.height >= 3.0 * line);
    assert_eq!(wrapped.size.height % line, 0.0, "whole lines");
    let narrower = bounds(LONG, 60.0, 0.0, lines(), &a);
    assert!(narrower.size.height > wrapped.size.height);

    // A word too long for a line breaks inside.
    let word = bounds("Supercalifragilisticexpialidocious", 50.0, 0.0, lines(), &a);
    assert!(word.size.width <= 50.0);
    assert!(word.size.height >= 3.0 * line);

    // A height limit keeps the lines that fit.
    let limited = bounds(LONG, 100.0, 2.5 * line, lines(), &a);
    assert_eq!(limited.size.height, 2.0 * line);
    assert!(limited.size.width <= 100.0);
    let truncated = bounds(LONG, 100.0, 2.5 * line, lines() | NSStringDrawingOptions::TruncatesLastVisibleLine, &a);
    assert_eq!(truncated.size.height, 2.0 * line);
    assert!(truncated.size.width <= 100.0);
}

fn line_break_modes() {
    let font = helvetica(12.0);
    let line = size("Hello", &attrs(&font)).height;
    for mode in [
        NSLineBreakMode::ByTruncatingTail,
        NSLineBreakMode::ByTruncatingHead,
        NSLineBreakMode::ByTruncatingMiddle,
        NSLineBreakMode::ByClipping,
    ] {
        let a = attrs_with_style(&font, &style(|s| s.setLineBreakMode(mode)));
        // One line per paragraph, however narrow.
        let r = bounds(LONG, 100.0, 0.0, lines(), &a);
        assert_eq!(r.size.height, line, "{mode:?}");
        assert!(r.size.width <= 100.0, "{mode:?}");
        assert_eq!(bounds("Hello\nWorld", 100.0, 0.0, lines(), &a).size.height, 2.0 * line);
    }
    let clip = attrs_with_style(&font, &style(|s| s.setLineBreakMode(NSLineBreakMode::ByClipping)));
    assert_eq!(bounds(LONG, 100.0, 0.0, lines(), &clip).size.width, 100.0, "clipped text fills the width");
    let tail = attrs_with_style(&font, &style(|s| s.setLineBreakMode(NSLineBreakMode::ByTruncatingTail)));
    assert!(bounds(LONG, 100.0, 0.0, lines(), &tail).size.width > 50.0, "truncation keeps what fits");

    let chars = attrs_with_style(&font, &style(|s| s.setLineBreakMode(NSLineBreakMode::ByCharWrapping)));
    let r = bounds(LONG, 100.0, 0.0, lines(), &chars);
    assert!(r.size.width <= 100.0);
    assert!(r.size.height >= 3.0 * line);
}

fn spacing() {
    let font = helvetica(12.0);
    let line = size("Hello", &attrs(&font)).height;
    let measure =
        |text: &str, style: &NSParagraphStyle| bounds(text, 0.0, 0.0, lines(), &attrs_with_style(&font, style));
    let two = "Hello\nWorld";

    let s = style(|s| s.setLineSpacing(10.0));
    assert_eq!(measure("Hello", &s).size.height, line, "no spacing after the last line");
    assert_eq!(measure(two, &s).size.height, 2.0 * line + 10.0);
    let wrapped = bounds(LONG, 100.0, 0.0, lines(), &attrs_with_style(&font, &s)).size.height;
    let plain = bounds(LONG, 100.0, 0.0, lines(), &attrs(&font)).size.height;
    let count = plain / line;
    assert_eq!(wrapped, plain + (count - 1.0) * 10.0, "between wrapped lines too");

    let s = style(|s| s.setParagraphSpacing(10.0));
    assert_eq!(measure(two, &s).size.height, 2.0 * line + 10.0);
    assert_eq!(
        bounds(LONG, 100.0, 0.0, lines(), &attrs_with_style(&font, &s)).size.height,
        plain,
        "only between paragraphs"
    );
    let s = style(|s| s.setParagraphSpacingBefore(10.0));
    assert_eq!(measure(two, &s).size.height, 2.0 * line + 10.0, "not before the first paragraph");

    let s = style(|s| s.setLineHeightMultiple(2.0));
    assert_eq!(measure(two, &s).size.height, 4.0 * line);
    let s = style(|s| s.setMinimumLineHeight(30.0));
    assert_eq!(measure(two, &s).size.height, 60.0);
    let s = style(|s| s.setMaximumLineHeight(10.0));
    assert_eq!(measure(two, &s).size.height, 20.0);
}

fn indents() {
    let font = helvetica(12.0);
    let plain = bounds(LONG, 100.0, 0.0, lines(), &attrs(&font));
    let head = bounds(LONG, 100.0, 0.0, lines(), &attrs_with_style(&font, &style(|s| s.setHeadIndent(20.0))));
    assert!(head.size.height >= plain.size.height, "less room, more lines");
    assert!(head.size.width <= 100.0);
    // An indent counts in the width of text that wraps, not of text that
    // fits on its lines anyway.
    let indented = attrs_with_style(&font, &style(|s| s.setFirstLineHeadIndent(50.0)));
    let wrapped = bounds("Hello there", 70.0, 0.0, lines(), &indented);
    assert!(wrapped.size.width > 50.0 && wrapped.size.width <= 70.0);
    assert!(wrapped.size.height > size("Hello", &attrs(&font)).height);
    assert_eq!(bounds("Hello", 0.0, 0.0, lines(), &indented).size, size("Hello", &attrs(&font)));
    let tail = bounds(LONG, 100.0, 0.0, lines(), &attrs_with_style(&font, &style(|s| s.setTailIndent(-30.0))));
    assert!(tail.size.width <= 70.5, "a negative tail indent comes off the right");
    assert!(tail.size.height > plain.size.height);
    let fixed = bounds(LONG, 100.0, 0.0, lines(), &attrs_with_style(&font, &style(|s| s.setTailIndent(50.0))));
    assert!(fixed.size.height > plain.size.height, "a positive tail indent is measured from the left");
}

fn alignment_within_width() {
    let font = helvetica(12.0);
    for alignment in
        [NSTextAlignment::Left, NSTextAlignment::Right, NSTextAlignment::Center, NSTextAlignment::Justified]
    {
        let a = attrs_with_style(&font, &style(|s| s.setAlignment(alignment)));
        let r = bounds(LONG, 150.0, 0.0, lines(), &a);
        assert!(r.size.width <= 150.0, "{alignment:?}");
        assert_eq!(r.size.height, bounds(LONG, 150.0, 0.0, lines(), &attrs(&font)).size.height, "{alignment:?}");
    }
}

fn tab_stops() {
    let font = NSFont::userFixedPitchFontOfSize(12.0).unwrap();
    let options = NSDictionary::new();
    // SAFETY: the options are empty.
    let tab = |alignment, location| unsafe {
        NSTextTab::initWithTextAlignment_location_options(NSTextTab::alloc(), alignment, location, &options)
    };
    let right = tab(NSTextAlignment::Right, 100.0);
    assert_eq!((right.location(), right.alignment()), (100.0, NSTextAlignment::Right));
    assert_eq!(right.tabStopType(), NSTextTabType::RightTabStopType);
    let center = NSTextTab::initWithType_location(NSTextTab::alloc(), NSTextTabType::CenterTabStopType, 50.0);
    assert_eq!((center.alignment(), center.tabStopType()), (NSTextAlignment::Center, NSTextTabType::CenterTabStopType));
    assert!(tab(NSTextAlignment::Left, 28.0).isEqual(Some(&tab(NSTextAlignment::Left, 28.0))));

    // The default stops are every 28 points.
    let plain = style(|_| {});
    let a = size("a", &attrs(&font)).width;
    let bbb = size("bbb", &attrs(&font)).width;
    assert!(close(size("a\tb", &attrs_with_style(&font, &plain)).width, 28.0 + a, 0.01));
    // Without them, a tab goes nowhere; with a default interval, to its
    // next multiple.
    let bare = style(|s| (1..=12).for_each(|i| s.removeTabStop(&tab(NSTextAlignment::Left, 28.0 * f64::from(i)))));
    assert!(close(size("a\tb", &attrs_with_style(&font, &bare)).width, 2.0 * a, 0.01));
    bare.setDefaultTabInterval(40.0);
    assert!(close(size("a\t\tb", &attrs_with_style(&font, &bare)).width, 80.0 + a, 0.01));
    // Text after a right tab ends on it; after a center tab, centers on it.
    bare.setDefaultTabInterval(0.0);
    bare.addTabStop(&right);
    assert!(close(size("a\tbbb", &attrs_with_style(&font, &bare)).width, 100.0, 0.01));
    bare.removeTabStop(&right);
    bare.addTabStop(&tab(NSTextAlignment::Center, 100.0));
    assert!(close(size("a\tbbb", &attrs_with_style(&font, &bare)).width, 100.0 + bbb / 2.0, 0.01));
}

type Test = (&'static str, fn());

fn main() {
    let tests: &[Test] = &[
        ("attribute_names", attribute_names),
        ("font_sizes", font_sizes),
        ("font_weights_and_traits", font_weights_and_traits),
        ("text_styles", text_styles),
        ("fixed_pitch", fixed_pitch),
        ("metrics", metrics),
        ("names", names),
        ("font_equality", font_equality),
        ("paragraph_defaults", paragraph_defaults),
        ("paragraph_mutation_and_copies", paragraph_mutation_and_copies),
        ("sizes", sizes),
        ("kerning", kerning),
        ("bounding_rects", bounding_rects),
        ("line_break_modes", line_break_modes),
        ("spacing", spacing),
        ("indents", indents),
        ("alignment_within_width", alignment_within_width),
        ("tab_stops", tab_stops),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test());
        println!("test {name} ... ok");
    }
}
