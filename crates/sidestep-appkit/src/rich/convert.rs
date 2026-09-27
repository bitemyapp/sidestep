//! Between [`Doc`]s and attributed strings: the objects a document's
//! styles become (fonts, colors, paragraph styles, shadows, links) and
//! back, and document attribute dictionaries.
//!
//! Fonts are found as AppKit finds them for rich text: by each name in turn
//! (`fontWithName:size:` takes PostScript, family and full names), then by
//! the name without a style suffix (`Helvetica-Bold` is Helvetica, bold),
//! then by the family class (Helvetica, Times, Courier or the system font);
//! bold and italic are then added through the font's descriptor. Colors are
//! made in their own space, a system color by its name when there is one.
//! Equal styles make one dictionary, so their runs are one run.

use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, Message};
#[allow(deprecated)] // NSObliqueness and NSExpansion, which rich text carries.
use objc2_app_kit::{
    NSAttachmentAttributeName, NSBackgroundColorAttributeName, NSBaselineOffsetAttributeName, NSColor, NSColorSpace,
    NSColorSpaceModel, NSColorType, NSExpansionAttributeName, NSFont, NSFontAttributeName,
    NSFontDescriptorSymbolicTraits, NSForegroundColorAttributeName, NSKernAttributeName, NSLigatureAttributeName,
    NSLinkAttributeName, NSMutableParagraphStyle, NSObliquenessAttributeName, NSParagraphStyle,
    NSParagraphStyleAttributeName, NSShadow, NSShadowAttributeName, NSStrikethroughColorAttributeName,
    NSStrikethroughStyleAttributeName, NSStrokeColorAttributeName, NSStrokeWidthAttributeName,
    NSSuperscriptAttributeName, NSTextAlignment, NSTextAttachment, NSTextTab, NSTextTabType,
    NSUnderlineColorAttributeName, NSUnderlineStyleAttributeName, NSWritingDirection,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSData, NSDictionary, NSFileWrapper, NSMutableAttributedString, NSNumber, NSRange,
    NSSize, NSString, NSURL, NSValue,
};

use super::keys;
use super::model::{
    Align, CharStyle, Color, Direction, Doc, DocAttrs, Font, Generic, ParaStyle, Shadow, Space, Tab, TabKind,
    paragraph_ends,
};

type Dict = NSDictionary<NSString, AnyObject>;

/// An object as `AnyObject`, for a dictionary's values.
pub(super) fn any<T: Message>(object: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every object is an `AnyObject`.
    unsafe { Retained::cast_unchecked(object) }
}

// Documents to attributed strings.

/// The attributed string `doc` stands for.
pub(crate) fn to_attributed(doc: &Doc) -> Retained<NSMutableAttributedString> {
    let string = NSMutableAttributedString::from_nsstring(&NSString::from_str(&doc.text));
    let mut maker = Maker::default();
    // Cut the text where a run or a paragraph ends, and give each piece the
    // dictionary of its run's style and its paragraph's.
    let runs: Vec<_> = doc.run_ranges().collect();
    let paras: Vec<_> = doc.para_ranges().collect();
    let (mut r, mut p, mut at, mut unit) = (0, 0, 0, 0);
    let mut pieces: Vec<(NSRange, Retained<Dict>)> = Vec::new();
    while at < doc.text.len() {
        while runs[r].0.end <= at {
            r += 1;
        }
        while paras[p].0.end <= at {
            p += 1;
        }
        let end = runs[r].0.end.min(paras[p].0.end);
        let units = doc.text[at..end].encode_utf16().count();
        let dict = maker.dictionary(runs[r].1, paras[p].1);
        match pieces.last_mut() {
            Some((range, last)) if Retained::as_ptr(last) == Retained::as_ptr(&dict) => range.length += units,
            _ => pieces.push((NSRange::new(unit, units), dict)),
        }
        (at, unit) = (end, unit + units);
    }
    for (range, dict) in pieces {
        // SAFETY: the dictionary holds attribute values of the right kinds.
        unsafe { string.setAttributes_range(Some(&dict), range) };
    }
    // Each attachment on its character: an attachment of its file.
    for a in &doc.attachments {
        let Some(at) = doc.text.get(..a.at).map(|t| t.encode_utf16().count()) else { continue };
        let wrapper =
            NSFileWrapper::initRegularFileWithContents(NSFileWrapper::alloc(), &NSData::with_bytes(&a.contents));
        if !a.name.is_empty() {
            wrapper.setPreferredFilename(Some(&NSString::from_str(&a.name)));
        }
        let attachment = NSTextAttachment::initWithFileWrapper(NSTextAttachment::alloc(), Some(&wrapper));
        // SAFETY: the key is a constant string AppKit exports; an
        // attachment is its value.
        unsafe { string.addAttribute_value_range(NSAttachmentAttributeName, &attachment, NSRange::new(at, 1)) };
    }
    string
}

/// Objects made for a document, each style made once.
#[derive(Default)]
struct Maker {
    dicts: Vec<(CharStyle, Option<ParaStyle>, Retained<Dict>)>,
    fonts: Vec<(Font, Retained<NSFont>)>,
}

impl Maker {
    fn dictionary(&mut self, style: &CharStyle, para: Option<&ParaStyle>) -> Retained<Dict> {
        if let Some((_, _, d)) = self.dicts.iter().find(|(s, p, _)| s == style && p.as_ref() == para) {
            return d.clone();
        }
        let mut keys: Vec<&NSString> = Vec::new();
        let mut values: Vec<Retained<AnyObject>> = Vec::new();
        let mut put = |key: &'static NSString, value: Retained<AnyObject>| {
            keys.push(key);
            values.push(value);
        };
        let double = |v: f64| any(NSNumber::numberWithDouble(v));
        let integer = |v: i64| any(NSNumber::numberWithInteger(v as isize));
        let color = |c: &Color| any(make_color(c));
        // SAFETY: the keys are constant strings AppKit exports.
        unsafe {
            if let Some(f) = &style.font {
                let font = self.font(f);
                put(NSFontAttributeName, any(font));
            }
            if let Some(p) = para {
                put(NSParagraphStyleAttributeName, any(make_paragraph(p)));
            }
            if let Some(c) = &style.color {
                put(NSForegroundColorAttributeName, color(c));
            }
            if let Some(c) = &style.background {
                put(NSBackgroundColorAttributeName, color(c));
            }
            if style.underline != 0 {
                put(NSUnderlineStyleAttributeName, integer(style.underline));
            }
            if let Some(c) = &style.underline_color {
                put(NSUnderlineColorAttributeName, color(c));
            }
            if style.strikethrough != 0 {
                put(NSStrikethroughStyleAttributeName, integer(style.strikethrough));
            }
            if let Some(c) = &style.strikethrough_color {
                put(NSStrikethroughColorAttributeName, color(c));
            }
            if let Some(link) = &style.link {
                put(NSLinkAttributeName, make_link(link));
            }
            if style.baseline_offset != 0.0 {
                put(NSBaselineOffsetAttributeName, double(style.baseline_offset));
            }
            if style.superscript != 0 {
                put(NSSuperscriptAttributeName, integer(style.superscript));
            }
            if let Some(k) = style.kern {
                put(NSKernAttributeName, double(k));
            }
            if let Some(l) = style.ligature {
                put(NSLigatureAttributeName, integer(l));
            }
            if let Some(s) = &style.shadow {
                put(NSShadowAttributeName, any(make_shadow(s)));
            }
            #[allow(deprecated)]
            if style.expansion != 0.0 {
                put(NSExpansionAttributeName, double(style.expansion));
            }
            #[allow(deprecated)]
            if style.obliqueness != 0.0 {
                put(NSObliquenessAttributeName, double(style.obliqueness));
            }
            if style.stroke_width != 0.0 {
                put(NSStrokeWidthAttributeName, double(style.stroke_width));
            }
            if let Some(c) = &style.stroke_color {
                put(NSStrokeColorAttributeName, color(c));
            }
        }
        let refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
        let dict = NSDictionary::from_slices(&keys, &refs);
        self.dicts.push((style.clone(), para.cloned(), dict.clone()));
        dict
    }

    fn font(&mut self, f: &Font) -> Retained<NSFont> {
        if let Some((_, font)) = self.fonts.iter().find(|(k, _)| k == f) {
            return font.clone();
        }
        let font = make_font(f);
        self.fonts.push((f.clone(), font.clone()));
        font
    }
}

/// The font `f` asks for (see the module's documentation).
pub(crate) fn make_font(f: &Font) -> Retained<NSFont> {
    let size = if f.size > 0.0 { f.size } else { 12.0 };
    let by_name = |name: &str| NSFont::fontWithName_size(&NSString::from_str(name), size);
    let mut found: Option<(Retained<NSFont>, bool, bool)> = None;
    for name in &f.names {
        if let Some(font) = by_name(name) {
            found = Some((font, false, false));
            break;
        }
    }
    if found.is_none() {
        // A PostScript name whose face isn't here: its family, in the
        // style its suffix names.
        for name in &f.names {
            if let Some((family, style)) = name.rsplit_once('-')
                && let Some(font) = by_name(family.trim_end_matches("PSMT").trim_end_matches("MT"))
            {
                let style = style.to_ascii_lowercase();
                let bold = ["bold", "black", "heavy", "semibold", "demi"].iter().any(|w| style.contains(w));
                let italic = style.contains("italic") || style.contains("oblique");
                found = Some((font, bold, italic));
                break;
            }
        }
    }
    let (font, bold, italic) = found.unwrap_or_else(|| {
        let font = match f.generic {
            Generic::Sans => by_name("Helvetica"),
            Generic::Serif => by_name("Times"),
            Generic::Mono => by_name("Courier"),
            Generic::System => None,
        };
        (font.unwrap_or_else(|| NSFont::systemFontOfSize(size)), false, false)
    });
    let (bold, italic) = (bold || f.bold, italic || f.italic);
    if !bold && !italic {
        return font;
    }
    let descriptor = font.fontDescriptor();
    let mut traits = descriptor.symbolicTraits();
    if bold {
        traits |= NSFontDescriptorSymbolicTraits::TraitBold;
    }
    if italic {
        traits |= NSFontDescriptorSymbolicTraits::TraitItalic;
    }
    let styled = descriptor.fontDescriptorWithSymbolicTraits(traits);
    NSFont::fontWithDescriptor_size(&styled, size).unwrap_or(font)
}

/// The color `c` names.
pub(crate) fn make_color(c: &Color) -> Retained<NSColor> {
    if let Some(name) = &c.name
        && let Some(named) =
            NSColor::colorWithCatalogName_colorName(&NSString::from_str("System"), &NSString::from_str(name))
    {
        return named;
    }
    let v = |i: usize| c.components.get(i).copied().unwrap_or(0.0);
    let a = c.alpha();
    match c.space {
        Space::Srgb => NSColor::colorWithSRGBRed_green_blue_alpha(v(0), v(1), v(2), a),
        Space::GenericRgb => NSColor::colorWithCalibratedRed_green_blue_alpha(v(0), v(1), v(2), a),
        Space::DisplayP3 => NSColor::colorWithDisplayP3Red_green_blue_alpha(v(0), v(1), v(2), a),
        Space::Gray => NSColor::colorWithGenericGamma22White_alpha(v(0), a),
        Space::Cmyk => NSColor::colorWithDeviceCyan_magenta_yellow_black_alpha(v(0), v(1), v(2), v(3), a),
    }
}

fn make_paragraph(p: &ParaStyle) -> Retained<NSParagraphStyle> {
    let style = NSMutableParagraphStyle::new();
    style.setAlignment(match p.alignment {
        Align::Left => NSTextAlignment::Left,
        Align::Right => NSTextAlignment::Right,
        Align::Center => NSTextAlignment::Center,
        Align::Justified => NSTextAlignment::Justified,
        Align::Natural => NSTextAlignment::Natural,
    });
    style.setFirstLineHeadIndent(p.first_line_head_indent);
    style.setHeadIndent(p.head_indent);
    style.setTailIndent(p.tail_indent);
    style.setLineSpacing(p.line_spacing);
    style.setParagraphSpacing(p.paragraph_spacing);
    style.setParagraphSpacingBefore(p.paragraph_spacing_before);
    style.setMinimumLineHeight(p.minimum_line_height);
    style.setMaximumLineHeight(p.maximum_line_height);
    style.setLineHeightMultiple(p.line_height_multiple);
    style.setBaseWritingDirection(match p.direction {
        Direction::Natural => NSWritingDirection::Natural,
        Direction::LeftToRight => NSWritingDirection::LeftToRight,
        Direction::RightToLeft => NSWritingDirection::RightToLeft,
    });
    let tabs: Vec<Retained<NSTextTab>> = p.tabs.iter().map(make_tab).collect();
    style.setTabStops(Some(&NSArray::from_retained_slice(&tabs)));
    style.setDefaultTabInterval(p.default_tab_interval);
    style.setHeaderLevel(p.header_level as isize);
    style.setAllowsDefaultTighteningForTruncation(p.tightening);
    Retained::into_super(style)
}

fn make_tab(t: &Tab) -> Retained<NSTextTab> {
    let kind = match t.kind {
        TabKind::Left => NSTextTabType::LeftTabStopType,
        TabKind::Right => NSTextTabType::RightTabStopType,
        TabKind::Center => NSTextTabType::CenterTabStopType,
        TabKind::Decimal => NSTextTabType::DecimalTabStopType,
    };
    NSTextTab::initWithType_location(NSTextTab::alloc(), kind, t.location)
}

fn make_shadow(s: &Shadow) -> Retained<NSShadow> {
    let shadow = NSShadow::new();
    shadow.setShadowOffset(NSSize::new(s.offset.0, s.offset.1));
    shadow.setShadowBlurRadius(s.blur);
    shadow.setShadowColor(s.color.as_ref().map(make_color).as_deref());
    shadow
}

/// A link's value: a URL where the text makes one (spaces escaped, as
/// AppKit escapes them), else the text.
fn make_link(link: &str) -> Retained<AnyObject> {
    let url = NSURL::URLWithString(&NSString::from_str(link))
        .or_else(|| NSURL::URLWithString(&NSString::from_str(&link.replace(' ', "%20"))));
    match url {
        Some(url) => any(url),
        None => any(NSString::from_str(link)),
    }
}

// Attributed strings to documents.

/// The part `range` of `string` as a document: its runs' styles, and each
/// paragraph in the paragraph style of its first character (or none).
pub(crate) fn from_attributed(string: &NSAttributedString, range: NSRange) -> Doc {
    let part = if range.location == 0 && range.length == string.length() {
        string.retain()
    } else {
        string.attributedSubstringFromRange(range)
    };
    let mut doc = doc_of(&part);
    doc.attachments = attachments_of(&part, &doc.text);
    doc
}

/// The attachments of `string` (whose text is `text`): each U+FFFC with an
/// attachment whose file wrapper holds a file, as that file (named
/// uniquely), at the size the attachment lays out at.
fn attachments_of(string: &NSAttributedString, text: &str) -> Vec<super::model::Attachment> {
    let mut out: Vec<super::model::Attachment> = Vec::new();
    let mut unit = 0;
    let mut from = 0;
    for (at, _) in text.match_indices('\u{FFFC}') {
        unit += text[from..at].encode_utf16().count();
        from = at;
        // SAFETY: the key is a constant string AppKit exports; an index in
        // the string.
        let value =
            unsafe { string.attribute_atIndex_effectiveRange(NSAttachmentAttributeName, unit, std::ptr::null_mut()) };
        let Some(value) = value else { continue };
        let Some(attachment) = value.downcast_ref::<NSTextAttachment>() else { continue };
        let Some(wrapper) = attachment.fileWrapper().filter(|w| w.isRegularFile()) else { continue };
        let Some(contents) = wrapper.regularFileContents() else { continue };
        let name = wrapper
            .preferredFilename()
            .or_else(|| wrapper.filename())
            .map_or_else(|| "Attachment".into(), |n| n.to_string());
        let name = unique_name(&name, |n| out.iter().any(|a| a.name == n));
        let metrics = crate::attachment::at_index(unit, || crate::attachment::metrics(&value, None, 0.0, 0.0));
        let size = metrics.map_or((0.0, 0.0), |m| (f64::from(m.width), f64::from(m.height)));
        out.push(super::model::Attachment {
            at,
            name,
            contents: sidestep_foundation::data::to_vec(&contents),
            width: size.0,
            height: size.1,
        });
    }
    out
}

/// `name`, or with a number after its stem (`Attachment 2.png`) if `taken`
/// says it is.
fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (2..).map(|n| format!("{stem} {n}{ext}")).find(|n| !taken(n)).unwrap_or_else(|| name.to_owned())
}

/// The runs and paragraphs of `part`, as a document.
fn doc_of(part: &NSAttributedString) -> Doc {
    sidestep_foundation::with_runs(part, |text, refs| {
        let mut styles: HashMap<*const Dict, (CharStyle, Option<ParaStyle>)> = HashMap::new();
        let mut runs: Vec<(usize, CharStyle)> = Vec::new();
        // Where each run starts, and its paragraph style.
        let mut starts: Vec<(usize, Option<ParaStyle>)> = Vec::new();
        for r in refs.iter().filter(|r| !r.utf8.is_empty()) {
            let (style, para) = styles.entry(Retained::as_ptr(&r.attrs)).or_insert_with(|| styles_of(&r.attrs)).clone();
            starts.push((r.utf8.start, para));
            match runs.last_mut() {
                Some((end, last)) if *last == style => *end = r.utf8.end,
                _ => runs.push((r.utf8.end, style)),
            }
        }
        let mut paras = Vec::new();
        let mut start = 0;
        for end in paragraph_ends(text) {
            let i = starts.partition_point(|(s, _)| *s <= start).saturating_sub(1);
            paras.push((end, starts.get(i).and_then(|(_, p)| p.clone())));
            start = end;
        }
        if runs.is_empty() {
            runs.push((0, CharStyle::default()));
        }
        Doc { text: text.to_owned(), runs, paras, attrs: DocAttrs::default(), attachments: Vec::new() }
    })
}

/// The character style and paragraph style an attribute dictionary holds.
fn styles_of(dict: &Dict) -> (CharStyle, Option<ParaStyle>) {
    let get = |key: &NSString| dict.objectForKey(key);
    let number = |key: &NSString| get(key).and_then(|v| crate::font::number(&v));
    let color = |key: &NSString| get(key).and_then(|v| v.downcast::<NSColor>().ok()).map(|c| color_of(&c));
    // SAFETY: the keys are constant strings AppKit exports.
    unsafe {
        let font = get(NSFontAttributeName).and_then(|v| v.downcast::<NSFont>().ok()).map(|f| font_of(&f));
        let para = get(NSParagraphStyleAttributeName)
            .and_then(|v| v.downcast::<NSParagraphStyle>().ok())
            .map(|p| paragraph_of(&p));
        let link = get(NSLinkAttributeName).and_then(|v| match v.downcast::<NSURL>() {
            Ok(url) => url.absoluteString().map(|s| s.to_string()),
            Err(v) => v.downcast::<NSString>().ok().map(|s| s.to_string()),
        });
        let shadow = get(NSShadowAttributeName).and_then(|v| v.downcast::<NSShadow>().ok()).map(|s| Shadow {
            offset: (s.shadowOffset().width, s.shadowOffset().height),
            blur: s.shadowBlurRadius(),
            color: s.shadowColor().map(|c| color_of(&c)),
        });
        #[allow(deprecated)]
        let (expansion, obliqueness) = (number(NSExpansionAttributeName), number(NSObliquenessAttributeName));
        let style = CharStyle {
            font,
            color: color(NSForegroundColorAttributeName),
            background: color(NSBackgroundColorAttributeName),
            underline: number(NSUnderlineStyleAttributeName).unwrap_or(0.0) as i64,
            underline_color: color(NSUnderlineColorAttributeName),
            strikethrough: number(NSStrikethroughStyleAttributeName).unwrap_or(0.0) as i64,
            strikethrough_color: color(NSStrikethroughColorAttributeName),
            link,
            baseline_offset: number(NSBaselineOffsetAttributeName).unwrap_or(0.0),
            superscript: number(NSSuperscriptAttributeName).unwrap_or(0.0) as i64,
            kern: number(NSKernAttributeName),
            ligature: number(NSLigatureAttributeName).map(|l| l as i64),
            shadow,
            expansion: expansion.unwrap_or(0.0),
            obliqueness: obliqueness.unwrap_or(0.0),
            stroke_width: number(NSStrokeWidthAttributeName).unwrap_or(0.0),
            stroke_color: color(NSStrokeColorAttributeName),
        };
        (style, para)
    }
}

/// A font as rich text names it.
pub(crate) fn font_of(font: &NSFont) -> Font {
    let traits = font.fontDescriptor().symbolicTraits();
    let name = font.fontName().to_string();
    let family = font.familyName().map(|f| f.to_string()).unwrap_or_default();
    let generic = if name.starts_with('.') {
        Generic::System
    } else if font.isFixedPitch() || traits.contains(NSFontDescriptorSymbolicTraits::TraitMonoSpace) {
        Generic::Mono
    } else if is_serif(&family) {
        Generic::Serif
    } else {
        Generic::Sans
    };
    Font {
        names: vec![name],
        family,
        generic,
        size: font.pointSize(),
        bold: traits.contains(NSFontDescriptorSymbolicTraits::TraitBold),
        italic: traits.contains(NSFontDescriptorSymbolicTraits::TraitItalic),
    }
}

/// Whether a family is a serif one, by its name.
fn is_serif(family: &str) -> bool {
    let f = family.to_ascii_lowercase();
    if f.contains("sans") {
        return false;
    }
    f.contains("serif")
        || [
            "times",
            "georgia",
            "palatino",
            "garamond",
            "baskerville",
            "cambria",
            "bookman",
            "century",
            "charter",
            "didot",
            "bodoni",
            "hoefler",
            "new york",
        ]
        .iter()
        .any(|n| f.starts_with(n))
}

/// A color as rich text keeps it.
pub(crate) fn color_of(c: &NSColor) -> Color {
    let srgb = c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()).map_or([0.0, 0.0, 0.0, 1.0], |s| {
        [s.redComponent(), s.greenComponent(), s.blueComponent(), s.alphaComponent()]
    });
    let named = |name: String| Color { space: Space::Srgb, components: srgb.to_vec(), name: Some(name), srgb };
    match c.r#type() {
        NSColorType::Catalog => return named(c.colorNameComponent().to_string()),
        NSColorType::ComponentBased => {}
        _ => return Color { space: Space::Srgb, components: srgb.to_vec(), name: None, srgb },
    }
    let space = c.colorSpace();
    let alpha = c.alphaComponent();
    let with = |space: Space, components: Vec<f64>| {
        let mut components = components;
        components.push(alpha);
        Color { space, components, name: None, srgb }
    };
    match space.colorSpaceModel() {
        NSColorSpaceModel::Gray => with(Space::Gray, vec![c.whiteComponent()]),
        NSColorSpaceModel::CMYK => {
            with(Space::Cmyk, vec![c.cyanComponent(), c.magentaComponent(), c.yellowComponent(), c.blackComponent()])
        }
        NSColorSpaceModel::RGB => {
            let rgb = vec![c.redComponent(), c.greenComponent(), c.blueComponent()];
            if space == NSColorSpace::genericRGBColorSpace() {
                with(Space::GenericRgb, rgb)
            } else if space == NSColorSpace::displayP3ColorSpace() {
                with(Space::DisplayP3, rgb)
            } else {
                with(Space::Srgb, srgb[..3].to_vec())
            }
        }
        _ => with(Space::Srgb, srgb[..3].to_vec()),
    }
}

/// A paragraph style as rich text keeps it.
pub(crate) fn paragraph_of(p: &NSParagraphStyle) -> ParaStyle {
    let alignment = match p.alignment() {
        a if a == NSTextAlignment::Left => Align::Left,
        a if a == NSTextAlignment::Right => Align::Right,
        a if a == NSTextAlignment::Center => Align::Center,
        a if a == NSTextAlignment::Justified => Align::Justified,
        _ => Align::Natural,
    };
    let direction = match p.baseWritingDirection() {
        NSWritingDirection::LeftToRight => Direction::LeftToRight,
        NSWritingDirection::RightToLeft => Direction::RightToLeft,
        _ => Direction::Natural,
    };
    let tabs = p
        .tabStops()
        .iter()
        .map(|t| Tab {
            location: t.location(),
            kind: match t.tabStopType() {
                NSTextTabType::RightTabStopType => TabKind::Right,
                NSTextTabType::CenterTabStopType => TabKind::Center,
                NSTextTabType::DecimalTabStopType => TabKind::Decimal,
                _ => TabKind::Left,
            },
        })
        .collect();
    ParaStyle {
        alignment,
        first_line_head_indent: p.firstLineHeadIndent(),
        head_indent: p.headIndent(),
        tail_indent: p.tailIndent(),
        line_spacing: p.lineSpacing(),
        paragraph_spacing: p.paragraphSpacing(),
        paragraph_spacing_before: p.paragraphSpacingBefore(),
        minimum_line_height: p.minimumLineHeight(),
        maximum_line_height: p.maximumLineHeight(),
        line_height_multiple: p.lineHeightMultiple(),
        direction,
        tabs,
        default_tab_interval: p.defaultTabInterval(),
        header_level: p.headerLevel() as i64,
        tightening: p.allowsDefaultTighteningForTruncation(),
    }
}

// Document attributes.

/// The format a document was read from, for its attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Plain,
    Rtf,
    Rtfd,
    Html,
}

/// The document attributes of a document read from `format`, as AppKit
/// reports them (measured): for RTF, the page (US Letter and RTF's margins
/// unless the document gives its own), the default tab interval,
/// hyphenation, text scaling and the Cocoa RTF version (80 for RTF that
/// doesn't name one), and whatever else the document set; for HTML, the
/// type alone (and the Cocoa version a Cocoa HTML writer names); for plain
/// text, the type and the encoding it was read in.
pub(crate) fn document_attributes(a: &DocAttrs, format: Format, encoding: Option<usize>) -> Retained<Dict> {
    let mut entries: Vec<(&str, Retained<AnyObject>)> = Vec::new();
    let string = |s: &str| any(NSString::from_str(s));
    let double = |v: f64| any(NSNumber::numberWithDouble(v));
    let integer = |v: i64| any(NSNumber::numberWithInteger(v as isize));
    // SAFETY: NSValue's size constructor.
    let size = |(w, h): (f64, f64)| any(unsafe { NSValue::valueWithSize(NSSize::new(w, h)) });
    let (kind, uti) = match format {
        Format::Plain => ("NSPlainText", "public.plain-text"),
        Format::Rtf => ("NSRTF", "public.rtf"),
        Format::Rtfd => ("NSRTFD", "com.apple.rtfd"),
        Format::Html => ("NSHTML", "public.html"),
    };
    entries.push((keys::DOCUMENT_TYPE, string(kind)));
    match format {
        Format::Rtf | Format::Rtfd => {
            entries.push((keys::FILE_TYPE, string(uti)));
            entries.push((keys::COCOA_VERSION, double(a.cocoa_version.unwrap_or(80.0))));
            entries.push((keys::PAPER_SIZE, size(a.paper_size.unwrap_or(super::model::PAPER))));
            let m = super::model::MARGINS;
            entries.push((keys::LEFT_MARGIN, double(a.left_margin.unwrap_or(m[0]))));
            entries.push((keys::RIGHT_MARGIN, double(a.right_margin.unwrap_or(m[1]))));
            entries.push((keys::TOP_MARGIN, double(a.top_margin.unwrap_or(m[2]))));
            entries.push((keys::BOTTOM_MARGIN, double(a.bottom_margin.unwrap_or(m[3]))));
            entries.push((keys::DEFAULT_TAB_INTERVAL, double(a.default_tab_interval.unwrap_or(0.0))));
            entries.push((keys::HYPHENATION_FACTOR, double(a.hyphenation_factor.unwrap_or(0.0))));
            entries.push((keys::TEXT_SCALING, integer(a.text_scaling.unwrap_or(0))));
            entries.push((keys::USES_SCREEN_FONTS, integer(0)));
        }
        Format::Html => {
            if let Some(v) = a.cocoa_version {
                entries.push((keys::COCOA_VERSION, double(v)));
            }
        }
        Format::Plain => {
            if let Some(e) = encoding {
                entries.push((keys::CHARACTER_ENCODING, integer(e as i64)));
            }
        }
    }
    if let Some(v) = a.view_size {
        entries.push((keys::VIEW_SIZE, size(v)));
    }
    if let Some(v) = a.view_zoom {
        entries.push((keys::VIEW_ZOOM, double(v)));
    }
    if let Some(v) = a.view_mode {
        entries.push((keys::VIEW_MODE, integer(v)));
    }
    if let Some(v) = a.read_only {
        entries.push((keys::READ_ONLY, integer(v)));
    }
    if let Some(c) = &a.background {
        entries.push((keys::BACKGROUND_COLOR, any(make_color(c))));
    }
    for (key, value) in [
        (keys::TITLE, &a.title),
        (keys::AUTHOR, &a.author),
        (keys::SUBJECT, &a.subject),
        (keys::COMMENT, &a.comment),
        (keys::COMPANY, &a.company),
        (keys::COPYRIGHT, &a.copyright),
        (keys::EDITOR, &a.editor),
        (keys::MANAGER, &a.manager),
        (keys::CATEGORY, &a.category),
    ] {
        if let Some(v) = value {
            entries.push((key, string(v)));
        }
    }
    if !a.keywords.is_empty() {
        let words: Vec<Retained<NSString>> = a.keywords.iter().map(|k| NSString::from_str(k)).collect();
        entries.push((keys::KEYWORDS, any(NSArray::from_retained_slice(&words))));
    }
    let names: Vec<Retained<NSString>> = entries.iter().map(|(k, _)| NSString::from_str(k)).collect();
    let name_refs: Vec<&NSString> = names.iter().map(|n| &**n).collect();
    let values: Vec<&AnyObject> = entries.iter().map(|(_, v)| &**v).collect();
    NSDictionary::from_slices(&name_refs, &values)
}

/// A value of `dict` by its key's name.
pub(crate) fn value(dict: Option<&Dict>, key: &str) -> Option<Retained<AnyObject>> {
    dict?.objectForKey(&NSString::from_str(key))
}

/// A string value of `dict`.
pub(crate) fn string_value(dict: Option<&Dict>, key: &str) -> Option<String> {
    value(dict, key)?.downcast::<NSString>().ok().map(|s| s.to_string())
}

/// A number value of `dict`.
pub(crate) fn number_value(dict: Option<&Dict>, key: &str) -> Option<f64> {
    let v = value(dict, key)?;
    crate::font::number(&v)
}

/// The document attributes a writer is given.
pub(crate) fn doc_attrs_of(dict: Option<&Dict>) -> DocAttrs {
    let size = |key: &str| {
        let v = value(dict, key)?.downcast::<NSValue>().ok()?;
        // SAFETY: NSValue's size accessor; a value holding something else
        // answers what it can, as on macOS.
        let s = unsafe { v.sizeValue() };
        Some((s.width, s.height))
    };
    let keywords = value(dict, keys::KEYWORDS)
        .map(|v| {
            crate::font::array_items(&v)
                .iter()
                .filter_map(|k| k.downcast_ref::<NSString>().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    DocAttrs {
        paper_size: size(keys::PAPER_SIZE),
        left_margin: number_value(dict, keys::LEFT_MARGIN),
        right_margin: number_value(dict, keys::RIGHT_MARGIN),
        top_margin: number_value(dict, keys::TOP_MARGIN),
        bottom_margin: number_value(dict, keys::BOTTOM_MARGIN),
        view_size: size(keys::VIEW_SIZE),
        view_zoom: number_value(dict, keys::VIEW_ZOOM),
        view_mode: number_value(dict, keys::VIEW_MODE).map(|v| v as i64),
        read_only: number_value(dict, keys::READ_ONLY).map(|v| v as i64),
        hyphenation_factor: number_value(dict, keys::HYPHENATION_FACTOR),
        default_tab_interval: number_value(dict, keys::DEFAULT_TAB_INTERVAL),
        cocoa_version: None,
        text_scaling: None,
        title: string_value(dict, keys::TITLE),
        author: string_value(dict, keys::AUTHOR),
        subject: string_value(dict, keys::SUBJECT),
        keywords,
        comment: string_value(dict, keys::COMMENT),
        company: string_value(dict, keys::COMPANY),
        copyright: string_value(dict, keys::COPYRIGHT),
        editor: string_value(dict, keys::EDITOR),
        manager: string_value(dict, keys::MANAGER),
        category: string_value(dict, keys::CATEGORY),
        background: value(dict, keys::BACKGROUND_COLOR)
            .and_then(|v| v.downcast::<NSColor>().ok())
            .map(|c| color_of(&c)),
    }
}
