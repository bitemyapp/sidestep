//! Attributed strings read and written as RTF, RTFD, HTML and plain text
//! (`initWithData:options:documentAttributes:error:`,
//! `dataFromRange:documentAttributes:error:`, `RTFFromRange:…` and the
//! rest), their document attributes, and attributed strings on the
//! pasteboard, on macOS and on Linux alike.
//!
//! RTF and HTML are checked for the structure AppKit writes (not whole
//! bytes: the fonts' names differ between systems, and the Cocoa version
//! with macOS), for what survives being written and read back, and for
//! what hand-written RTF and the HTML browsers put on the pasteboard read
//! as. HTML is read through WebKit on macOS, so only what a program relies
//! on is checked there: traits, sizes relative to the text's, links, list
//! markers, white space.
//!
//! Pasteboards are made with `pasteboardWithUniqueName`, never the
//! general one. AppKit belongs to the main thread (WebKit reads HTML
//! there), so this file has its own `main`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, ClassType, MainThreadMarker, msg_send};
#[allow(deprecated)] // NSObliqueness, which rich text carries.
use objc2_app_kit::{
    NSAttributedStringAppKitDocumentFormats, NSAttributedStringDocumentFormats, NSBaselineOffsetAttributeName,
    NSCharacterEncodingDocumentAttribute, NSCharacterEncodingDocumentOption, NSColor, NSColorSpace,
    NSDefaultAttributesDocumentOption, NSDefaultTabIntervalDocumentAttribute, NSDocumentTypeDocumentAttribute,
    NSDocumentTypeDocumentOption, NSFont, NSFontAttributeName, NSFontDescriptorSymbolicTraits,
    NSForegroundColorAttributeName, NSHTMLTextDocumentType, NSKernAttributeName, NSLeftMarginDocumentAttribute,
    NSLinkAttributeName, NSMutableAttributedStringDocumentFormats, NSMutableParagraphStyle, NSObliquenessAttributeName,
    NSPaperSizeDocumentAttribute, NSParagraphStyle, NSParagraphStyleAttributeName, NSPasteboard, NSPasteboardReading,
    NSPasteboardTypeHTML, NSPasteboardTypeRTF, NSPasteboardTypeString, NSPasteboardWriting, NSPlainTextDocumentType,
    NSRTFDTextDocumentType, NSRTFTextDocumentType, NSReadOnlyDocumentAttribute, NSShadow, NSShadowAttributeName,
    NSStrikethroughStyleAttributeName, NSStrokeWidthAttributeName, NSSuperscriptAttributeName, NSTextAlignment,
    NSTextTab, NSTextTabType, NSTitleDocumentAttribute, NSUnderlineStyleAttributeName, NSWritingDirection,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSData, NSDictionary, NSError, NSMutableAttributedString, NSNumber, NSRange, NSSize,
    NSString, NSURL, NSValue,
};

use sidestep as _;

type Dict = NSDictionary<NSString, AnyObject>;

fn dict(pairs: &[(&NSString, &AnyObject)]) -> Retained<Dict> {
    let keys: Vec<&NSString> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<&AnyObject> = pairs.iter().map(|p| p.1).collect();
    NSDictionary::from_slices(&keys, &values)
}

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn helvetica(size: f64) -> Retained<NSFont> {
    NSFont::userFontOfSize(size).expect("the user font")
}

fn with_traits(font: &NSFont, traits: NSFontDescriptorSymbolicTraits) -> Retained<NSFont> {
    let d = font.fontDescriptor();
    let d = d.fontDescriptorWithSymbolicTraits(d.symbolicTraits() | traits);
    NSFont::fontWithDescriptor_size(&d, font.pointSize()).expect("a styled font")
}

fn traits(font: &NSFont) -> NSFontDescriptorSymbolicTraits {
    font.fontDescriptor().symbolicTraits()
}

fn srgb(r: f64, g: f64, b: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
}

fn components(c: &NSColor) -> [f64; 4] {
    let c = c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()).expect("an sRGB color");
    [c.redComponent(), c.greenComponent(), c.blueComponent(), c.alphaComponent()]
}

fn bytes(data: &NSData) -> Vec<u8> {
    data.to_vec()
}

fn text_of(data: &NSData) -> String {
    String::from_utf8(bytes(data)).expect("UTF-8")
}

/// A value of an attribute at `i`.
fn at(string: &NSAttributedString, i: usize, key: &NSString) -> Option<Retained<AnyObject>> {
    // SAFETY: an index inside the string.
    unsafe { string.attribute_atIndex_effectiveRange(key, i, std::ptr::null_mut()) }
}

fn font_at(string: &NSAttributedString, i: usize) -> Retained<NSFont> {
    // SAFETY: a constant key.
    at(string, i, unsafe { NSFontAttributeName }).expect("a font").downcast::<NSFont>().unwrap()
}

fn number_at(string: &NSAttributedString, i: usize, key: &NSString) -> Option<f64> {
    at(string, i, key).map(|v| v.downcast::<NSNumber>().unwrap().doubleValue())
}

fn options(kind: &NSString) -> Retained<Dict> {
    // SAFETY: a constant key.
    dict(&[(unsafe { NSDocumentTypeDocumentOption }, kind)])
}

fn attributes(kind: &NSString) -> Retained<Dict> {
    // SAFETY: a constant key.
    dict(&[(unsafe { NSDocumentTypeDocumentAttribute }, kind)])
}

/// Read `data` with `options`: the string and its document attributes.
fn read(data: &[u8], options: &Dict) -> Result<(Retained<NSAttributedString>, Retained<Dict>), Retained<NSError>> {
    let mut attrs = None;
    // SAFETY: options of the right kinds, and a place for the attributes.
    let string = unsafe {
        NSAttributedString::initWithData_options_documentAttributes_error(
            NSAttributedString::alloc(),
            &NSData::with_bytes(data),
            options,
            Some(&mut attrs),
        )
    }?;
    Ok((string, attrs.expect("document attributes")))
}

fn write(string: &NSAttributedString, attrs: &Dict) -> Result<Retained<NSData>, Retained<NSError>> {
    // SAFETY: attributes of the right kinds.
    unsafe { string.dataFromRange_documentAttributes_error(NSRange::new(0, string.length()), attrs) }
}

fn rtf(string: &NSAttributedString) -> String {
    // SAFETY: an empty dictionary of attributes.
    let data =
        unsafe { string.RTFFromRange_documentAttributes(NSRange::new(0, string.length()), &NSDictionary::new()) };
    text_of(&data.expect("RTF"))
}

fn read_rtf(text: &str) -> (Retained<NSAttributedString>, Retained<Dict>) {
    // SAFETY: a constant.
    read(text.as_bytes(), &options(unsafe { NSRTFTextDocumentType })).expect("RTF reads")
}

fn read_html(text: &str) -> Retained<NSAttributedString> {
    // SAFETY: a constant.
    let o = options(unsafe { NSHTMLTextDocumentType });
    read(format!("<meta charset=\"utf-8\">{text}").as_bytes(), &o).expect("HTML reads").0
}

fn doc_value(attrs: &Dict, key: &NSString) -> Option<Retained<AnyObject>> {
    attrs.objectForKey(key)
}

fn doc_number(attrs: &Dict, key: &NSString) -> Option<f64> {
    doc_value(attrs, key).map(|v| v.downcast::<NSNumber>().unwrap().doubleValue())
}

fn doc_string(attrs: &Dict, key: &NSString) -> Option<String> {
    doc_value(attrs, key).map(|v| v.downcast::<NSString>().unwrap().to_string())
}

/// The keys, types and options have AppKit's values.
fn constants(_: MainThreadMarker) {
    // SAFETY: constant strings.
    let pairs: &[(&NSString, &str)] = unsafe {
        &[
            (NSPlainTextDocumentType, "NSPlainText"),
            (NSRTFTextDocumentType, "NSRTF"),
            (NSRTFDTextDocumentType, "NSRTFD"),
            (NSHTMLTextDocumentType, "NSHTML"),
            (NSDocumentTypeDocumentAttribute, "DocumentType"),
            (NSDocumentTypeDocumentOption, "DocumentType"),
            (NSCharacterEncodingDocumentAttribute, "CharacterEncoding"),
            (NSCharacterEncodingDocumentOption, "CharacterEncoding"),
            (NSDefaultAttributesDocumentOption, "DefaultAttributes"),
            (NSPaperSizeDocumentAttribute, "PaperSize"),
            (NSLeftMarginDocumentAttribute, "LeftMargin"),
            (NSReadOnlyDocumentAttribute, "ReadOnly"),
            (NSDefaultTabIntervalDocumentAttribute, "DefaultTabInterval"),
            (NSTitleDocumentAttribute, "NSTitleDocumentAttribute"),
            (objc2_app_kit::NSCocoaVersionDocumentAttribute, "CocoaRTFVersion"),
            (objc2_app_kit::NSFileTypeDocumentAttribute, "UTI"),
            (objc2_app_kit::NSBaseURLDocumentOption, "BaseURL"),
            (objc2_app_kit::NSTextEncodingNameDocumentOption, "TextEncodingName"),
            (objc2_app_kit::NSViewModeDocumentAttribute, "ViewMode"),
            (objc2_app_kit::NSKeywordsDocumentAttribute, "NSKeywordsDocumentAttribute"),
            (objc2_app_kit::NSDefaultFontExcludedDocumentAttribute, "NoDefaultFonts"),
            (objc2_app_kit::NSWebArchiveTextDocumentType, "NSWebArchive"),
            (objc2_app_kit::NSDocFormatTextDocumentType, "NSDocFormat"),
        ]
    };
    for (constant, value) in pairs {
        assert_eq!(constant.to_string(), *value);
    }
}

/// A sample: "Plain Bold red link\nCentered" in Helvetica 12 with a bold
/// run, a red one, a link, and a centered second paragraph.
fn sample() -> Retained<NSMutableAttributedString> {
    let string = NSMutableAttributedString::from_nsstring(&s("Plain Bold red link\nCentered"));
    let font = helvetica(12.0);
    let bold = with_traits(&font, NSFontDescriptorSymbolicTraits::TraitBold);
    let centered = NSMutableParagraphStyle::new();
    centered.setAlignment(NSTextAlignment::Center);
    let url = NSURL::URLWithString(&s("https://example.com/")).unwrap();
    // SAFETY: constant keys and values of their kinds, over the text.
    unsafe {
        string.setAttributes_range(Some(&dict(&[(NSFontAttributeName, &font)])), NSRange::new(0, string.length()));
        string.addAttribute_value_range(NSFontAttributeName, &bold, NSRange::new(6, 4));
        string.addAttribute_value_range(NSForegroundColorAttributeName, &srgb(1.0, 0.0, 0.0), NSRange::new(11, 3));
        string.addAttribute_value_range(NSLinkAttributeName, &url, NSRange::new(15, 4));
        string.addAttribute_value_range(NSParagraphStyleAttributeName, &centered, NSRange::new(20, 8));
    }
    string
}

/// The sample, read back from some format.
fn check_sample(back: &NSAttributedString) {
    assert_eq!(back.string().to_string().trim_end_matches('\n'), "Plain Bold red link\nCentered");
    assert!(traits(&font_at(back, 7)).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    assert!(!traits(&font_at(back, 1)).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    assert_eq!(font_at(back, 1).pointSize(), 12.0);
    // SAFETY: constant keys.
    let (color, link, style) =
        unsafe { (NSForegroundColorAttributeName, NSLinkAttributeName, NSParagraphStyleAttributeName) };
    let red = components(&at(back, 12, color).unwrap().downcast::<NSColor>().unwrap());
    assert!(red[0] > 0.95 && red[1] < 0.05 && red[2] < 0.05, "{red:?}");
    assert!(at(back, 1, color).is_none());
    let url = at(back, 16, link).expect("a link").downcast::<NSURL>().expect("a URL");
    assert_eq!(url.absoluteString().unwrap().to_string(), "https://example.com/");
    assert!(at(back, 1, link).is_none());
    let para = at(back, 21, style).unwrap().downcast::<NSParagraphStyle>().unwrap();
    assert_eq!(para.alignment(), NSTextAlignment::Center);
}

/// RTF is laid out as AppKit writes it.
fn rtf_writes_as_appkit_does(_: MainThreadMarker) {
    let plain = rtf(&NSAttributedString::from_nsstring(&s("plain no attrs")));
    assert!(plain.starts_with("{\\rtf1\\ansi\\ansicpg1252\\cocoartf"), "{plain}");
    assert!(plain.contains("\n\\cocoatextscaling0\\cocoaplatform0{\\fonttbl\\f0\\f"), "{plain}");
    assert!(plain.contains("}\n{\\colortbl;\\red255\\green255\\blue255;}\n{\\*\\expandedcolortbl;;}\n"), "{plain}");
    let tabs = "\\tx560\\tx1120\\tx1680\\tx2240\\tx2800\\tx3360\\tx3920\\tx4480\\tx5040\\tx5600\\tx6160\\tx6720";
    assert!(
        plain
            .ends_with(&format!("\\pard{tabs}\\pardirnatural\\partightenfactor0\n\n\\f0\\fs24 \\cf0 plain no attrs}}")),
        "{plain}"
    );
    let sample = rtf(&sample());
    assert!(sample.contains("{\\*\\expandedcolortbl;;\\cssrgb\\c100000\\c0\\c0;}"), "{sample}");
    assert!(
        sample.contains(
            "\\cf2 red\\cf0  {\\field{\\*\\fldinst{HYPERLINK \"https://example.com/\"}}{\\fldrslt link}}\\\n"
        ),
        "{sample}"
    );
    assert!(sample.contains("\\pardirnatural\\qc\\partightenfactor0\n"), "{sample}");
    assert!(sample.contains("\\b Bold\n"), "{sample}");
    let unicode = rtf(&NSAttributedString::from_nsstring(&s("café € 日本 \\{}\ttab")));
    assert!(unicode.contains("caf\\'e9 \\'80 \\uc0\\u26085 \\u26412  \\\\\\{\\}\ttab}"), "{unicode}");
    let indented = NSMutableParagraphStyle::new();
    indented.setFirstLineHeadIndent(36.0);
    indented.setHeadIndent(18.0);
    indented.setTailIndent(-20.0);
    let string = NSMutableAttributedString::from_nsstring(&s("x"));
    // SAFETY: a constant key and a paragraph style.
    unsafe { string.addAttribute_value_range(NSParagraphStyleAttributeName, &indented, NSRange::new(0, 1)) };
    assert!(rtf(&string).contains(&format!("\\pard{tabs}\\li360\\fi360\\ri400\\pardirnatural")));
}

/// What RTF can say comes back when written and read.
#[allow(deprecated)]
fn rtf_round_trips(_: MainThreadMarker) {
    check_sample(&read_rtf(&rtf(&sample())).0);
    let string = NSMutableAttributedString::from_nsstring(&s("0 1 2 3 4 5 6 7 8"));
    let shadow = NSShadow::new();
    shadow.setShadowOffset(NSSize::new(2.0, -2.0));
    shadow.setShadowBlurRadius(3.0);
    shadow.setShadowColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.5)));
    let n = |v: f64| NSNumber::numberWithDouble(v);
    let i = |v: isize| NSNumber::numberWithInteger(v);
    let italic = with_traits(&helvetica(18.5), NSFontDescriptorSymbolicTraits::TraitItalic);
    // SAFETY: constant keys and values of their kinds, over the text.
    unsafe {
        string.setAttributes_range(
            Some(&dict(&[(NSFontAttributeName, &helvetica(12.0))])),
            NSRange::new(0, string.length()),
        );
        string.addAttribute_value_range(NSUnderlineStyleAttributeName, &i(9), NSRange::new(0, 1));
        string.addAttribute_value_range(NSStrikethroughStyleAttributeName, &i(1), NSRange::new(2, 1));
        string.addAttribute_value_range(NSSuperscriptAttributeName, &i(1), NSRange::new(4, 1));
        string.addAttribute_value_range(NSBaselineOffsetAttributeName, &n(3.0), NSRange::new(6, 1));
        string.addAttribute_value_range(NSKernAttributeName, &n(1.5), NSRange::new(8, 1));
        string.addAttribute_value_range(NSShadowAttributeName, &shadow, NSRange::new(10, 1));
        string.addAttribute_value_range(NSObliquenessAttributeName, &n(0.25), NSRange::new(12, 1));
        string.addAttribute_value_range(NSStrokeWidthAttributeName, &n(-3.0), NSRange::new(14, 1));
        string.addAttribute_value_range(NSFontAttributeName, &italic, NSRange::new(16, 1));
    }
    let (back, _) = read_rtf(&rtf(&string));
    assert_eq!(back.string().to_string(), "0 1 2 3 4 5 6 7 8");
    // SAFETY: constant keys.
    unsafe {
        assert_eq!(number_at(&back, 0, NSUnderlineStyleAttributeName), Some(9.0));
        assert_eq!(number_at(&back, 2, NSStrikethroughStyleAttributeName), Some(1.0));
        assert_eq!(number_at(&back, 4, NSSuperscriptAttributeName), Some(1.0));
        assert_eq!(number_at(&back, 6, NSBaselineOffsetAttributeName), Some(3.0));
        assert_eq!(number_at(&back, 8, NSKernAttributeName), Some(1.5));
        assert_eq!(number_at(&back, 12, NSObliquenessAttributeName), Some(0.25));
        assert_eq!(number_at(&back, 14, NSStrokeWidthAttributeName), Some(-3.0));
        assert_eq!(number_at(&back, 1, NSUnderlineStyleAttributeName), None);
        let shadow = at(&back, 10, NSShadowAttributeName).unwrap().downcast::<NSShadow>().unwrap();
        assert_eq!((shadow.shadowOffset(), shadow.shadowBlurRadius()), (NSSize::new(2.0, -2.0), 3.0));
        assert!((components(&shadow.shadowColor().unwrap())[3] - 0.5).abs() < 0.01);
    }
    let big = font_at(&back, 16);
    assert!(traits(&big).contains(NSFontDescriptorSymbolicTraits::TraitItalic));
    assert_eq!(big.pointSize(), 18.5);
    // Paragraph styles.
    let style = NSMutableParagraphStyle::new();
    style.setAlignment(NSTextAlignment::Right);
    style.setFirstLineHeadIndent(36.0);
    style.setHeadIndent(18.0);
    style.setTailIndent(-20.0);
    style.setParagraphSpacing(10.0);
    style.setParagraphSpacingBefore(5.0);
    style.setLineSpacing(3.0);
    style.setMinimumLineHeight(15.0);
    style.setMaximumLineHeight(30.0);
    style.setLineHeightMultiple(1.5);
    style.setDefaultTabInterval(40.0);
    let tabs: Vec<Retained<NSTextTab>> = [
        (NSTextTabType::LeftTabStopType, 50.0),
        (NSTextTabType::RightTabStopType, 150.0),
        (NSTextTabType::CenterTabStopType, 200.0),
        (NSTextTabType::DecimalTabStopType, 250.0),
    ]
    .into_iter()
    .map(|(t, l)| NSTextTab::initWithType_location(NSTextTab::alloc(), t, l))
    .collect();
    style.setTabStops(Some(&NSArray::from_retained_slice(&tabs)));
    let rtl = NSMutableParagraphStyle::new();
    rtl.setBaseWritingDirection(NSWritingDirection::RightToLeft);
    let string = NSMutableAttributedString::from_nsstring(&s("styled\nrtl"));
    // SAFETY: a constant key and paragraph styles over paragraphs.
    unsafe {
        string.addAttribute_value_range(NSParagraphStyleAttributeName, &style, NSRange::new(0, 7));
        string.addAttribute_value_range(NSParagraphStyleAttributeName, &rtl, NSRange::new(7, 3));
    }
    let (back, _) = read_rtf(&rtf(&string));
    // SAFETY: a constant key.
    let para =
        |i| at(&back, i, unsafe { NSParagraphStyleAttributeName }).unwrap().downcast::<NSParagraphStyle>().unwrap();
    let p = para(0);
    assert_eq!(p.alignment(), NSTextAlignment::Right);
    assert_eq!((p.firstLineHeadIndent(), p.headIndent()), (36.0, 18.0));
    assert_eq!(p.tailIndent(), 412.0, "measured from the leading margin: the page's 432 points less 20");
    assert_eq!((p.paragraphSpacing(), p.paragraphSpacingBefore(), p.lineSpacing()), (10.0, 5.0, 3.0));
    assert_eq!((p.minimumLineHeight(), p.maximumLineHeight(), p.lineHeightMultiple()), (15.0, 30.0, 1.5));
    assert_eq!(p.defaultTabInterval(), 40.0);
    let kinds: Vec<(NSTextTabType, f64)> = p.tabStops().iter().map(|t| (t.tabStopType(), t.location())).collect();
    assert_eq!(
        kinds,
        [
            (NSTextTabType::LeftTabStopType, 50.0),
            (NSTextTabType::RightTabStopType, 150.0),
            (NSTextTabType::CenterTabStopType, 200.0),
            (NSTextTabType::DecimalTabStopType, 250.0)
        ]
    );
    let r = para(8);
    assert_eq!((r.baseWritingDirection(), r.alignment()), (NSWritingDirection::RightToLeft, NSTextAlignment::Right));
    // Characters beyond ASCII, and RTF's own.
    let text = "café € “q” — 日本語 😀 \\ { } \ttab\u{2028}line";
    let (back, _) = read_rtf(&rtf(&NSAttributedString::from_nsstring(&s(text))));
    assert_eq!(back.string().to_string(), text);
}

/// Document attributes: those AppKit reports for RTF, and those written.
fn rtf_document_attributes(_: MainThreadMarker) {
    let (_, attrs) = read_rtf(&rtf(&NSAttributedString::from_nsstring(&s("x"))));
    // SAFETY: constant keys.
    unsafe {
        assert_eq!(doc_string(&attrs, NSDocumentTypeDocumentAttribute).as_deref(), Some("NSRTF"));
        let paper = doc_value(&attrs, NSPaperSizeDocumentAttribute).unwrap().downcast::<NSValue>().unwrap().sizeValue();
        assert_eq!(paper, NSSize::new(612.0, 792.0));
        assert_eq!(doc_number(&attrs, NSLeftMarginDocumentAttribute), Some(90.0));
        assert_eq!(doc_number(&attrs, objc2_app_kit::NSRightMarginDocumentAttribute), Some(90.0));
        assert_eq!(doc_number(&attrs, objc2_app_kit::NSTopMarginDocumentAttribute), Some(72.0));
        assert_eq!(doc_number(&attrs, objc2_app_kit::NSBottomMarginDocumentAttribute), Some(72.0));
        assert_eq!(doc_number(&attrs, NSDefaultTabIntervalDocumentAttribute), Some(0.0));
        assert!(doc_number(&attrs, objc2_app_kit::NSCocoaVersionDocumentAttribute).is_some_and(|v| v > 1000.0));
        assert_eq!(doc_string(&attrs, objc2_app_kit::NSFileTypeDocumentAttribute).as_deref(), Some("public.rtf"));
        assert!(doc_value(&attrs, NSTitleDocumentAttribute).is_none());
        // Written, and read back.
        let keywords = NSArray::from_retained_slice(&[s("k1"), s("k2")]);
        let given = dict(&[
            (NSTitleDocumentAttribute, &*s("T")),
            (objc2_app_kit::NSAuthorDocumentAttribute, &*s("A")),
            (objc2_app_kit::NSKeywordsDocumentAttribute, &*keywords),
            (NSPaperSizeDocumentAttribute, &*NSValue::valueWithSize(NSSize::new(500.0, 700.0))),
            (NSLeftMarginDocumentAttribute, &*NSNumber::numberWithDouble(50.0)),
            (NSReadOnlyDocumentAttribute, &*NSNumber::numberWithInteger(1)),
            (objc2_app_kit::NSViewModeDocumentAttribute, &*NSNumber::numberWithInteger(1)),
            (NSDefaultTabIntervalDocumentAttribute, &*NSNumber::numberWithDouble(30.0)),
        ]);
        let string = NSAttributedString::from_nsstring(&s("doc"));
        let data = string.RTFFromRange_documentAttributes(NSRange::new(0, 3), &given).unwrap();
        let (_, attrs) = read(&bytes(&data), &options(NSRTFTextDocumentType)).unwrap();
        assert_eq!(doc_string(&attrs, NSTitleDocumentAttribute).as_deref(), Some("T"));
        assert_eq!(doc_string(&attrs, objc2_app_kit::NSAuthorDocumentAttribute).as_deref(), Some("A"));
        let words = doc_value(&attrs, objc2_app_kit::NSKeywordsDocumentAttribute).unwrap();
        let words: Vec<String> = words
            .downcast::<NSArray>()
            .unwrap()
            .iter()
            .map(|w| w.downcast::<NSString>().unwrap().to_string())
            .collect();
        assert_eq!(words, ["k1", "k2"]);
        let paper = doc_value(&attrs, NSPaperSizeDocumentAttribute).unwrap().downcast::<NSValue>().unwrap().sizeValue();
        assert_eq!(paper, NSSize::new(500.0, 700.0));
        assert_eq!(doc_number(&attrs, NSLeftMarginDocumentAttribute), Some(50.0));
        assert_eq!(doc_number(&attrs, NSReadOnlyDocumentAttribute), Some(1.0));
        assert_eq!(doc_number(&attrs, objc2_app_kit::NSViewModeDocumentAttribute), Some(1.0));
        assert_eq!(doc_number(&attrs, NSDefaultTabIntervalDocumentAttribute), Some(30.0));
    }
    // RTF from other writers: half an inch between default tabs.
    let (_, attrs) = read_rtf("{\\rtf1 x}");
    // SAFETY: a constant key.
    assert_eq!(doc_number(&attrs, unsafe { NSDefaultTabIntervalDocumentAttribute }), Some(36.0));
}

/// RTF as other programs write it.
fn rtf_from_other_writers(_: MainThreadMarker) {
    let (back, _) = read_rtf(
        "{\\rtf1\\ansi\\ansicpg1252\\deff0{\\fonttbl{\\f0\\fswiss\\fcharset0 Helvetica;}{\\f1\\froman Times New Roman;}}\
         {\\colortbl;\\red255\\green0\\blue0;}{\\stylesheet{\\s0 Normal;}}{\\*\\generator Some Writer}\
         {\\info{\\title Tt}}\\pard\\f0\\fs24 Hello \\b bold\\b0  \\i it\\i0  \\ul u\\ulnone  \\cf1 red\\cf0 \\par \
         Next \\'e9\\u233?x\\line y\\tab z {\\*\\unknown skip} {\\field{\\*\\fldinst HYPERLINK \"http://a.example/\"}{\\fldrslt here}}\\par\
         \\f1 serif}",
    );
    assert_eq!(back.string().to_string(), "Hello bold it u red\nNext ééx\u{2028}y\tz  here\nserif");
    assert!(traits(&font_at(&back, 7)).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    assert!(traits(&font_at(&back, 12)).contains(NSFontDescriptorSymbolicTraits::TraitItalic));
    assert!(!traits(&font_at(&back, 1)).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    // SAFETY: constant keys.
    unsafe {
        assert_eq!(number_at(&back, 14, NSUnderlineStyleAttributeName), Some(1.0));
        let red = components(&at(&back, 17, NSForegroundColorAttributeName).unwrap().downcast::<NSColor>().unwrap());
        assert!(red[0] > 0.9 && red[2] < 0.1, "{red:?}");
        let link = at(&back, 37, NSLinkAttributeName).expect("a link").downcast::<NSURL>().unwrap();
        assert_eq!(link.absoluteString().unwrap().to_string(), "http://a.example/");
    }
}

/// Errors, and types told from the data.
fn errors_and_types(_: MainThreadMarker) {
    // SAFETY: constants.
    let (rtf_type, bogus) = (unsafe { NSRTFTextDocumentType }, s("bogus"));
    let code = |r: Result<(Retained<NSAttributedString>, Retained<Dict>), Retained<NSError>>| {
        let e = r.map(|_| ()).expect_err("an error");
        (e.domain().to_string(), e.code())
    };
    let cocoa = "NSCocoaErrorDomain".to_string();
    assert_eq!(code(read(b"garbage", &options(rtf_type))), (cocoa.clone(), 256));
    assert_eq!(code(read(b"{\\rtf1 unterminated {\\b bold", &options(rtf_type))), (cocoa.clone(), 259));
    assert_eq!(code(read(b"x", &options(&bogus))), (cocoa.clone(), 65806));
    let string = NSAttributedString::from_nsstring(&s("x"));
    let e = write(&string, &NSDictionary::new()).expect_err("no type");
    assert_eq!((e.domain().to_string(), e.code()), (cocoa, 66062));
    // SAFETY: data.
    let none = unsafe {
        NSAttributedString::initWithRTF_documentAttributes(
            NSAttributedString::alloc(),
            &NSData::with_bytes(b"nope"),
            None,
        )
    };
    assert!(none.is_none());
    for (data, kind, text) in [
        (&b"{\\rtf1 x}"[..], "NSRTF", "x"),
        (b"<html><body>x</body></html>", "NSHTML", "x"),
        (b"<b>x</b>", "NSPlainText", "<b>x</b>"),
        (b"  {\\rtf1 y}", "NSPlainText", "  {\\rtf1 y}"),
    ] {
        let (string, attrs) = read(data, &NSDictionary::new()).unwrap();
        // SAFETY: a constant key.
        assert_eq!(doc_string(&attrs, unsafe { NSDocumentTypeDocumentAttribute }).as_deref(), Some(kind));
        assert_eq!(string.string().to_string().trim_end(), text);
    }
}

/// Plain text: its encoding, its attributes.
fn plain_text(_: MainThreadMarker) {
    // SAFETY: constants.
    let (plain, encoding_key, default_key) =
        unsafe { (NSPlainTextDocumentType, NSCharacterEncodingDocumentAttribute, NSDefaultAttributesDocumentOption) };
    let (string, attrs) = read("café".as_bytes(), &options(plain)).unwrap();
    assert_eq!(string.string().to_string(), "café");
    assert_eq!(doc_number(&attrs, encoding_key), Some(4.0), "UTF-8");
    let font = font_at(&string, 0);
    assert_eq!(font.pointSize(), 12.0);
    // Not UTF-8: Mac OS Roman. A byte order mark: UTF-16.
    let (string, attrs) = read(&[0x63, 0x61, 0x66, 0xe9], &options(plain)).unwrap();
    assert_eq!((string.string().to_string(), doc_number(&attrs, encoding_key)), ("cafÈ".to_string(), Some(30.0)));
    let (string, attrs) = read(&[0xff, 0xfe, b'h', 0, b'i', 0], &options(plain)).unwrap();
    assert_eq!((string.string().to_string(), doc_number(&attrs, encoding_key)), ("hi".to_string(), Some(10.0)));
    // Default attributes.
    let big = helvetica(20.0);
    // SAFETY: constant keys.
    let defaults = dict(&[(unsafe { NSFontAttributeName }, &big)]);
    // SAFETY: a constant key.
    let o = dict(&[(unsafe { NSDocumentTypeDocumentOption }, plain), (default_key, &defaults)]);
    let (string, _) = read(b"x", &o).unwrap();
    assert_eq!(font_at(&string, 0).pointSize(), 20.0);
    // Written: UTF-8, or UTF-16 with its byte order mark.
    let string = NSAttributedString::from_nsstring(&s("hi"));
    assert_eq!(bytes(&write(&string, &attributes(plain)).unwrap()), b"hi");
    // SAFETY: a constant key.
    let utf16 =
        dict(&[(unsafe { NSDocumentTypeDocumentAttribute }, plain), (encoding_key, &NSNumber::numberWithInteger(10))]);
    assert_eq!(bytes(&write(&string, &utf16).unwrap()), [0xff, 0xfe, b'h', 0, b'i', 0]);
}

/// HTML is structured as AppKit writes it.
fn html_writes_as_appkit_does(_: MainThreadMarker) {
    // SAFETY: a constant.
    let html = text_of(&write(&sample(), &attributes(unsafe { NSHTMLTextDocumentType })).unwrap());
    assert!(html.starts_with("<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\""), "{html}");
    assert!(html.contains("<meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\">"), "{html}");
    assert!(html.contains("<style type=\"text/css\">\np.p1 {margin: 0.0px 0.0px 0.0px 0.0px; font: 12.0px "), "{html}");
    assert!(html.contains("<p class=\"p1\">Plain <b>Bold</b> <span class=\"s1\">red</span> <a href=\"https://example.com/\">link</a></p>"), "{html}");
    assert!(html.contains("text-align: center"), "{html}");
    assert!(html.ends_with("</body>\n</html>\n"), "{html}");
    let escaped = NSAttributedString::from_nsstring(&s("<&> \"q\""));
    // SAFETY: a constant.
    let html = text_of(&write(&escaped, &attributes(unsafe { NSHTMLTextDocumentType })).unwrap());
    assert!(html.contains("&lt;&amp;&gt; \"q\""), "{html}");
}

/// What HTML can say comes back when written and read.
fn html_round_trips(_: MainThreadMarker) {
    // SAFETY: a constant.
    let data = write(&sample(), &attributes(unsafe { NSHTMLTextDocumentType })).unwrap();
    // SAFETY: as above.
    let (back, attrs) = read(&bytes(&data), &options(unsafe { NSHTMLTextDocumentType })).unwrap();
    check_sample(&back);
    // SAFETY: a constant key.
    assert_eq!(doc_string(&attrs, unsafe { NSDocumentTypeDocumentAttribute }).as_deref(), Some("NSHTML"));
}

/// HTML as browsers and editors put it on the pasteboard.
fn html_from_browsers(_: MainThreadMarker) {
    let back = read_html("<p>Hi <b>there</b> <i>it</i> <u>u</u> <s>s</s></p><p>second</p>");
    assert_eq!(back.string().to_string(), "Hi there it u s\nsecond\n");
    assert!(traits(&font_at(&back, 4)).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    assert!(traits(&font_at(&back, 10)).contains(NSFontDescriptorSymbolicTraits::TraitItalic));
    let body = font_at(&back, 0).pointSize();
    assert_eq!(body, 12.0);
    // SAFETY: constant keys.
    unsafe {
        assert_eq!(number_at(&back, 12, NSUnderlineStyleAttributeName), Some(1.0));
        assert_eq!(number_at(&back, 14, NSStrikethroughStyleAttributeName), Some(1.0));
        let p = at(&back, 0, NSParagraphStyleAttributeName).unwrap().downcast::<NSParagraphStyle>().unwrap();
        assert_eq!(p.paragraphSpacing(), 12.0, "a paragraph's 1em margin after it");
    }
    // Headings, code, links, breaks, entities and white space.
    let back = read_html(
        "<h1>Title</h1><p>a   b<br>c &amp; &lt;d&gt; &eacute; <code>code</code> <a href=\"https://example.com/x\">link</a></p>",
    );
    // (A break in a paragraph keeps the paragraph: U+2028.)
    assert_eq!(back.string().to_string(), "Title\na b\u{2028}c & <d> é code link\n");
    let title = font_at(&back, 0);
    assert!(title.pointSize() >= 24.0 && traits(&title).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    assert!(font_at(&back, 20).isFixedPitch(), "code is monospaced");
    // SAFETY: a constant key.
    let link = at(&back, 26, unsafe { NSLinkAttributeName }).expect("a link").downcast::<NSURL>().unwrap();
    assert_eq!(link.absoluteString().unwrap().to_string(), "https://example.com/x");
    // Lists as text with markers, colors from style.
    let back = read_html("<ul><li>one</li><li>two</li></ul><span style=\"color: rgb(255, 0, 0)\">red</span>");
    let text = back.string().to_string();
    assert!(text.contains("•\tone\n") && text.contains("•\ttwo\n") && text.ends_with("red"), "{text:?}");
    let i = text.encode_utf16().count() - 2;
    // SAFETY: a constant key.
    let red =
        components(&at(&back, i, unsafe { NSForegroundColorAttributeName }).unwrap().downcast::<NSColor>().unwrap());
    assert!(red[0] > 0.95 && red[1] < 0.05, "{red:?}");
}

/// Lists as editors put them on the pasteboard: Google Docs, GitHub and
/// Confluence put `<p>`s in `<li>`s, which stay in the item's paragraph.
fn html_lists(_: MainThreadMarker) {
    let text = |html: &str| read_html(html).string().to_string();
    let style = |string: &NSAttributedString, i: usize| {
        // SAFETY: a constant key.
        at(string, i, unsafe { NSParagraphStyleAttributeName }).unwrap().downcast::<NSParagraphStyle>().unwrap()
    };
    let item = read_html("<ul><li><p>para item</p></li></ul>");
    assert_eq!(item.string().to_string(), "\t•\tpara item\n");
    assert_eq!((style(&item, 4).paragraphSpacing(), style(&item, 4).headIndent()), (12.0, 36.0));
    assert_eq!(text("<ul><li><div>div item</div></li></ul>"), "\t•\tdiv item\n");
    assert_eq!(text("<ul>\n<li>\n<p>one</p>\n</li>\n<li>\n<p>two</p>\n</li>\n</ul>"), "\t•\tone\n\t•\ttwo\n");
    // Blocks in an item are lines of its paragraph.
    assert_eq!(
        text("<ol><li><p>x</p><p>y</p></li><li>text<p>then p</p></li></ol>"),
        "\t1\tx\u{2028}y\n\t2\ttextthen p\n"
    );
    // What follows a nested list gets the item's marker again; an empty
    // item is its marker, one holding only a list has none.
    assert_eq!(
        text("<ul><li><p>one</p><ul><li>n</li></ul>after</li><li></li><li><ul><li>x</li></ul></li></ul>"),
        "\t•\tone\n\t◦\tn\n\t•\tafter\n\t•\t\n\t◦\tx\n"
    );
    // A nested item's first line starts at the margin (its tab goes to the
    // marker).
    let nested = read_html("<ul><li>a<ul><li>b</li></ul></li></ul>");
    let p = style(&nested, 6);
    assert_eq!((p.headIndent(), p.firstLineHeadIndent(), p.alignment()), (72.0, 0.0, NSTextAlignment::Left));
    // Items are indented by their depth whatever the margins around them;
    // a block in one gives its right margin.
    let quoted = read_html("<ul><li><blockquote>q</blockquote></li></ul>");
    let p = style(&quoted, 3);
    assert_eq!((p.headIndent(), p.firstLineHeadIndent(), p.tailIndent()), (36.0, 0.0, -40.0));
    assert_eq!(style(&read_html("<blockquote><ul><li>x</li></ul></blockquote>"), 3).headIndent(), 36.0);
    // Markers: letters by case, going round; Roman numerals to 3999; an
    // item's value ignored.
    assert_eq!(text("<ol type=\"A\"><li>a<li>b</ol>"), "\tA\ta\n\tB\tb\n");
    assert_eq!(text("<ol type=\"I\"><li>a<li>b</ol>"), "\tI\ta\n\tII\tb\n");
    assert_eq!(text("<ol type=\"a\" start=\"27\"><li>a</ol>"), "\ta\ta\n");
    assert_eq!(text("<ol type=\"i\" start=\"3999\"><li>a<li>b</ol>"), "\tmmmcmxcix\ta\n\t\tb\n");
    assert_eq!(text("<ol><li value=\"0\">a<li value=\"9\">b</ol>"), "\t1\ta\n\t2\tb\n");
}

/// Line breaks and spaces outside blocks, as a browser's fragment has
/// them.
fn html_breaks_and_spaces(_: MainThreadMarker) {
    let text = |html: &str| read_html(html).string().to_string();
    assert_eq!(text("<html><body>\n<!--StartFragment--><b>x</b> y<!--EndFragment-->\n</body></html>"), "x y");
    assert_eq!(text("a<br>b<br>"), "a\nb\n");
    assert_eq!(text("<p>a</p><br>"), "a\n\n");
    assert_eq!(text("<div>a<br></div>"), "a\n");
    assert_eq!(
        text(
            "<span class=\"Apple-converted-space\">&nbsp;</span>a<span class=\"Apple-converted-space\">&nbsp; </span>b&nbsp;c"
        ),
        " a  b\u{A0}c"
    );
    assert_eq!(font_at(&read_html("<font size=\"7\">x</font>"), 0).pointSize(), 37.0);
}

/// Numbers at the ends of their ranges, and past them, read as AppKit
/// reads them.
fn extreme_numbers(_: MainThreadMarker) {
    // SAFETY: a constant key.
    let key = unsafe { NSParagraphStyleAttributeName };
    let (back, _) = read_rtf("{\\rtf1\\pard\\li2147483647\\fi1 x\\par}");
    let p = at(&back, 0, key).unwrap().downcast::<NSParagraphStyle>().unwrap();
    assert_eq!(p.headIndent(), 2147483647.0 / 20.0);
    assert!((p.firstLineHeadIndent() - 2147483648.0 / 20.0).abs() < 1e-6);
    let (back, _) = read_rtf("{\\rtf1\\pard\\sl-2147483648 x\\par}");
    let p = at(&back, 0, key).unwrap().downcast::<NSParagraphStyle>().unwrap();
    assert_eq!(p.minimumLineHeight(), 2147483648.0 / 20.0);
    // All the digits are the parameter's (their low 32 bits).
    let (back, _) = read_rtf("{\\rtf1\\pard\\li99999999999 x\\par}");
    assert_eq!(back.string().to_string(), "x\n");
    let p = at(&back, 0, key).unwrap().downcast::<NSParagraphStyle>().unwrap();
    assert_eq!(p.headIndent(), 1215752191.0 / 20.0);
    let text = |html: &str| read_html(html).string().to_string();
    assert_eq!(font_at(&read_html("<font size=\"+9223372036854775807\">x</font>"), 0).pointSize(), 12.0);
    assert_eq!(text("<ol start=\"9223372036854775807\"><li>a<li>b</ol>"), "\t1\ta\n\t2\tb\n");
    assert_eq!(text("<ol start=\"2147483647\"><li>a<li>b</ol>"), "\t2147483647\ta\n\t2147483648\tb\n");
    assert_eq!(text("<ol type=\"i\" start=\"2000000\"><li>a</ol>"), "\t\ta\n");
    assert_eq!(text("<ol><li value=\"9223372036854775807\">a<li>b</ol>"), "\t1\ta\n\t2\tb\n");
}

/// RTF's default font: text before any `\f` is Helvetica, whatever `\deff`
/// says; `\plain` goes back to `\f0`.
fn rtf_default_font(_: MainThreadMarker) {
    let name = |rtf: &str, i: usize| font_at(&read_rtf(rtf).0, i).fontName().to_string();
    // (By the names the fonts have here: Linux's stand-ins for them.)
    let named = |family: &str| NSFont::fontWithName_size(&s(family), 12.0).unwrap().fontName().to_string();
    let (helvetica, courier) = (named("Helvetica"), named("Courier"));
    assert_ne!(helvetica, named("Times"));
    assert_eq!(name("{\\rtf1\\deff1{\\fonttbl{\\f0 Times;}{\\f1 Courier;}}x}", 0), helvetica);
    assert_eq!(name("{\\rtf1{\\fonttbl{\\f3 Times;}{\\f0 Courier;}}\\f3 a\\plain b}", 1), courier);
    assert_eq!(name("{\\rtf1{\\fonttbl{\\f3 Times;}{\\f4 Courier;}}\\f9 a}", 0), helvetica);
}

/// `initWithHTML:options:…` reads HTML whatever the options say; URLs
/// other than files' aren't read.
fn html_options_and_urls(_: MainThreadMarker) {
    // SAFETY: HTML data, options naming another type, and no place for
    // attributes.
    let string = unsafe {
        NSAttributedString::initWithHTML_options_documentAttributes(
            NSAttributedString::alloc(),
            &NSData::with_bytes(b"<b>x</b>"),
            &options(NSRTFTextDocumentType),
            None,
        )
    };
    assert_eq!(string.map(|s| s.string().to_string()).as_deref(), Some("x"));
    let path = std::env::temp_dir().join(format!("sidestep-rich-text-{}.txt", std::process::id()));
    std::fs::write(&path, "local").unwrap();
    let from = |url: &NSURL| {
        // SAFETY: a URL, no options, no place for attributes.
        unsafe {
            NSAttributedString::initWithURL_options_documentAttributes_error(
                NSAttributedString::alloc(),
                url,
                &NSDictionary::new(),
                None,
            )
        }
        .map(|s| s.string().to_string())
        .map_err(|e| (e.domain().to_string(), e.code()))
    };
    assert_eq!(from(&NSURL::from_file_path(&path).unwrap()), Ok("local".to_string()));
    let remote = NSURL::URLWithString(&s(&format!("https://example.invalid{}", path.display()))).unwrap();
    assert_eq!(from(&remote), Err(("NSCocoaErrorDomain".to_string(), 262)));
    let _ = std::fs::remove_file(&path);
}

/// RTFD: flat, as the pasteboard has it.
fn rtfd(_: MainThreadMarker) {
    let sample = sample();
    // SAFETY: an empty dictionary.
    let data =
        unsafe { sample.RTFDFromRange_documentAttributes(NSRange::new(0, sample.length()), &NSDictionary::new()) }
            .unwrap();
    assert!(bytes(&data).starts_with(b"rtfd\0\0\0\0"));
    // SAFETY: RTFD data.
    let back = unsafe { NSAttributedString::initWithRTFD_documentAttributes(NSAttributedString::alloc(), &data, None) }
        .unwrap();
    check_sample(&back);
    // SAFETY: a constant.
    let (_, attrs) = read(&bytes(&data), &NSDictionary::new()).unwrap();
    // SAFETY: a constant key.
    assert_eq!(doc_string(&attrs, unsafe { NSDocumentTypeDocumentAttribute }).as_deref(), Some("NSRTFD"));
}

/// A mutable attributed string reads a document in place: RTF onto its
/// end (AppKit's RTF reader appends), HTML and plain text over what it
/// held.
fn read_in_place(_: MainThreadMarker) {
    let string = NSMutableAttributedString::from_nsstring(&s(""));
    let data = NSData::with_bytes(rtf(&sample()).as_bytes());
    // SAFETY: RTF data and options.
    unsafe { string.readFromData_options_documentAttributes_error(&data, &options(NSRTFTextDocumentType), None) }
        .unwrap();
    check_sample(&string);
    let string = NSMutableAttributedString::from_nsstring(&s("old "));
    // SAFETY: as above.
    unsafe {
        string.readFromData_options_documentAttributes_error(
            &NSData::with_bytes(b"{\\rtf1 new}"),
            &options(NSRTFTextDocumentType),
            None,
        )
    }
    .unwrap();
    assert_eq!(string.string().to_string(), "old new");
    // SAFETY: as above.
    unsafe {
        string.readFromData_options_documentAttributes_error(
            &NSData::with_bytes(b"plain"),
            &options(NSPlainTextDocumentType),
            None,
        )
    }
    .unwrap();
    assert_eq!(string.string().to_string(), "plain");
    // SAFETY: as above.
    let e = unsafe {
        string.readFromData_options_documentAttributes_error(&NSData::with_bytes(b"x"), &options(&s("bogus")), None)
    };
    assert_eq!(e.unwrap_err().code(), 65806);
}

/// Attributed strings on a pasteboard: RTF before text, read back whole.
fn pasteboards(_: MainThreadMarker) {
    let pb = NSPasteboard::pasteboardWithUniqueName();
    let string = sample();
    let position = |types: &[String], t: &NSString| types.iter().position(|x| *x == t.to_string());
    // SAFETY: constants.
    let (rtf_type, html_type, text_type) =
        unsafe { (NSPasteboardTypeRTF, NSPasteboardTypeHTML, NSPasteboardTypeString) };
    let writable: Vec<String> = string.writableTypesForPasteboard(&pb).iter().map(|t| t.to_string()).collect();
    assert!(
        position(&writable, rtf_type) < position(&writable, text_type) && position(&writable, rtf_type) == Some(0),
        "{writable:?}"
    );
    let readable: Vec<String> =
        NSAttributedString::readableTypesForPasteboard(&pb).iter().map(|t| t.to_string()).collect();
    let rtfd = s("com.apple.flat-rtfd");
    let order: Vec<Option<usize>> =
        [&*rtfd, rtf_type, html_type, text_type].iter().map(|t| position(&readable, t)).collect();
    assert!(order.iter().all(Option::is_some) && order.windows(2).all(|w| w[0] < w[1]), "{readable:?}");
    let kind = |t: &NSString| NSAttributedString::readingOptionsForType_pasteboard(t, &pb).0;
    assert_eq!((kind(text_type), kind(rtf_type)), (1, 0), "text as a string, the rest as data");
    pb.clearContents();
    let objects = NSArray::from_slice(&[&*string as &AnyObject]);
    // SAFETY: an array of objects that write themselves.
    assert!(unsafe { msg_send![&*pb, writeObjects: &*objects] });
    let types: Vec<String> = pb.types().unwrap().iter().map(|t| t.to_string()).collect();
    assert!(
        position(&types, rtf_type) < position(&types, text_type) && position(&types, rtf_type).is_some(),
        "{types:?}"
    );
    let classes = NSArray::from_slice(&[<NSAttributedString as ClassType>::class()]);
    let read_back = || -> Retained<NSAttributedString> {
        // SAFETY: classes that read themselves, and no options.
        let objects: Retained<NSArray<AnyObject>> =
            unsafe { msg_send![&*pb, readObjectsForClasses: &*classes, options: std::ptr::null::<AnyObject>()] };
        objects.firstObject().expect("an object").downcast::<NSAttributedString>().expect("an attributed string")
    };
    check_sample(&read_back());
    assert_eq!(pb.stringForType(text_type).unwrap().to_string(), "Plain Bold red link\nCentered");
    // Text alone reads as an attributed string without attributes.
    pb.clearContents();
    pb.setString_forType(&s("plain"), text_type);
    let plain = read_back();
    assert_eq!(plain.string().to_string(), "plain");
    // SAFETY: an index in the string.
    assert_eq!(unsafe { plain.attributesAtIndex_effectiveRange(0, std::ptr::null_mut()) }.count(), 0);
    // HTML alone reads as HTML.
    pb.clearContents();
    pb.setData_forType(Some(&NSData::with_bytes(b"<meta charset=\"utf-8\"><p>html <b>b</b></p>")), html_type);
    let html = read_back();
    assert_eq!(html.string().to_string(), "html b\n");
    assert!(traits(&font_at(&html, 5)).contains(NSFontDescriptorSymbolicTraits::TraitBold));
    // SAFETY: the pasteboard is this test's own.
    let _: () = unsafe { msg_send![&*pb, releaseGlobally] };
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("constants", constants),
        ("rtf_writes_as_appkit_does", rtf_writes_as_appkit_does),
        ("rtf_round_trips", rtf_round_trips),
        ("rtf_document_attributes", rtf_document_attributes),
        ("rtf_from_other_writers", rtf_from_other_writers),
        ("errors_and_types", errors_and_types),
        ("plain_text", plain_text),
        ("html_writes_as_appkit_does", html_writes_as_appkit_does),
        ("html_round_trips", html_round_trips),
        ("html_from_browsers", html_from_browsers),
        ("html_lists", html_lists),
        ("html_breaks_and_spaces", html_breaks_and_spaces),
        ("extreme_numbers", extreme_numbers),
        ("rtf_default_font", rtf_default_font),
        ("html_options_and_urls", html_options_and_urls),
        ("rtfd", rtfd),
        ("read_in_place", read_in_place),
        ("pasteboards", pasteboards),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
