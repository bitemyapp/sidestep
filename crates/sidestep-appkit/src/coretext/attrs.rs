//! An attributed string's text and runs as CoreText lays them out: the
//! attributes string drawing reads (`string_drawing::attrs_of`: AppKit's
//! names, several of which are CoreText's too, such as `NSFont`, `NSKern`
//! and `NSParagraphStyle`), then CoreText's own on top: colors as
//! `CGColor`s (`kCTForegroundColorAttributeName` and its kin), the
//! context's fill color (`kCTForegroundColorFromContextAttributeName`),
//! tracking, a baseline offset, and a `CTParagraphStyle`. Text without a
//! font attribute is in Helvetica 12 (the 12-point interface font here),
//! and in black, as CoreText draws it whatever the fill color.

use std::collections::HashMap;
use std::ops::Range;

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSFont;
use objc2_core_foundation::{CFAttributedString, CFString};
use objc2_core_text::{
    kCTBackgroundColorAttributeName, kCTBaselineOffsetAttributeName, kCTForegroundColorAttributeName,
    kCTForegroundColorFromContextAttributeName, kCTParagraphStyleAttributeName, kCTStrokeColorAttributeName,
    kCTTrackingAttributeName, kCTUnderlineColorAttributeName,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSString};

use crate::coregraphics::color::CGColorImpl;
use crate::font::number;
use crate::protocol::Color;
use crate::text::fonts::{Design, FontSpec};
use crate::text::layout::{Attrs, Direction, Run};

pub(crate) type Dict = NSDictionary<NSString, AnyObject>;

/// What CoreText reads from a run's attributes beyond the layout's.
#[derive(Clone)]
pub(crate) struct Style {
    /// The attributes as given; none for text without any.
    pub dict: Option<Retained<Dict>>,
    /// The font the attributes give, or the default.
    pub font: Retained<NSFont>,
    /// Glyphs take the context's fill color.
    pub from_context: bool,
    /// `kCTTrackingAttributeName`: points added after each glyph (part of
    /// the letter spacing; reported as trailing whitespace at the end).
    pub tracking: f64,
    /// The stroke width, a percentage of the font size (positive outlines,
    /// negative fills and outlines).
    pub stroke_width: f64,
}

/// An attributed string's text, its attributes and runs over it.
pub(crate) struct Styled {
    pub text: String,
    pub attrs: Vec<Attrs>,
    pub styles: Vec<Style>,
    /// Byte runs over `text`, in order, and their UTF-16 ranges.
    pub runs: Vec<Run>,
    pub units: Vec<Range<usize>>,
    pub utf16_len: usize,
    /// The byte each UTF-16 unit starts at (a pair's second unit, its
    /// character's), and the text's length at the end.
    bytes: Vec<u32>,
    /// The first paragraph's base direction.
    pub direction: Direction,
}

/// The font text without a font attribute is in.
pub(crate) fn default_font() -> Retained<NSFont> {
    crate::font::make_font(FontSpec::system(Design::Default, 12.0))
}

fn cg_color(value: &AnyObject) -> Option<Color> {
    value.downcast_ref::<CGColorImpl>().map(CGColorImpl::resolve)
}

fn truthy(value: &AnyObject) -> bool {
    number(value).is_some_and(|n| n != 0.0)
}

/// The layout attributes and CoreText's extras of an attribute dictionary.
pub(crate) fn style_of(dict: Option<&Dict>) -> (Attrs, Style) {
    let mut attrs = crate::string_drawing::attrs_of(dict);
    let given = dict
        .and_then(|d| {
            // SAFETY: the constant is this crate's own.
            let key = crate::coretext::ns_string(unsafe { objc2_core_text::kCTFontAttributeName });
            d.objectForKey(key)
        })
        .and_then(|f| f.downcast::<NSFont>().ok());
    let has_font = given.is_some();
    let font = given.unwrap_or_else(default_font);
    let mut style =
        Style { dict: dict.map(Message::retain), font, from_context: false, tracking: 0.0, stroke_width: 0.0 };
    let Some(dict) = dict else { return (attrs, style) };
    // CoreText's own keys, looked up only if the dictionary has more than
    // the font (the common case has nothing else).
    if dict.count() <= usize::from(has_font) {
        style.stroke_width = f64::from(attrs.stroke.width);
        return (attrs, style);
    }
    // SAFETY: the constants are this crate's own strings.
    let get = |key: &CFString| dict.objectForKey(super::ns_string(key));
    // SAFETY: as above.
    unsafe {
        if let Some(c) = get(kCTForegroundColorAttributeName).as_deref().and_then(cg_color) {
            attrs.color = c;
        }
        style.from_context = get(kCTForegroundColorFromContextAttributeName).as_deref().is_some_and(truthy);
        if let Some(c) = get(kCTStrokeColorAttributeName).as_deref().and_then(cg_color) {
            attrs.stroke.color = Some(c);
        }
        if let Some(c) = get(kCTUnderlineColorAttributeName).as_deref().and_then(cg_color) {
            attrs.underline.color = Some(c);
        }
        if let Some(c) = get(kCTBackgroundColorAttributeName).as_deref().and_then(cg_color) {
            attrs.background = Some(c);
        }
        if let Some(offset) = get(kCTBaselineOffsetAttributeName).as_deref().and_then(number) {
            attrs.baseline_offset = offset as f32;
        }
        if let Some(tracking) = get(kCTTrackingAttributeName).as_deref().and_then(number) {
            style.tracking = tracking;
            // A kern of 0 still turns kerning off beside the tracking's
            // letter spacing (measured on macOS).
            if attrs.kern == Some(0.0) && tracking != 0.0 {
                let mut features: Vec<([u8; 4], u16)> = attrs.font.features.as_deref().unwrap_or_default().to_vec();
                features.push((*b"kern", 0));
                attrs.font.features = Some(features.into());
            }
            attrs.kern = Some(attrs.kern.unwrap_or(0.0) + tracking as f32);
        }
        if let Some(p) = get(kCTParagraphStyleAttributeName)
            .and_then(|v| v.downcast::<super::paragraph::CTParagraphStyleImpl>().ok())
        {
            attrs.paragraph = p.layout();
        }
    }
    style.stroke_width = f64::from(attrs.stroke.width);
    (attrs, style)
}

/// `string`'s parts (or of `range` of it, in UTF-16 units).
pub(crate) fn styled(string: &CFAttributedString) -> Styled {
    // SAFETY: a CFAttributedString is an NSAttributedString here.
    let string: &NSAttributedString = unsafe { &*(string as *const CFAttributedString).cast() };
    sidestep_foundation::with_runs(string, |text, refs| {
        let mut attrs = Vec::new();
        let mut styles = Vec::new();
        let mut seen: HashMap<*const Dict, u32> = HashMap::new();
        let mut runs = Vec::with_capacity(refs.len());
        let mut units = Vec::with_capacity(refs.len());
        for r in refs.iter().filter(|r| !r.utf8.is_empty()) {
            let index = *seen.entry(Retained::as_ptr(&r.attrs)).or_insert_with(|| {
                let (a, s) = style_of(Some(&r.attrs));
                attrs.push(a);
                styles.push(s);
                attrs.len() as u32 - 1
            });
            runs.push(Run { start: r.utf8.start, end: r.utf8.end, attrs: index });
            units.push(r.utf16.clone());
        }
        if runs.is_empty() {
            let (a, s) = style_of(None);
            attrs.push(a);
            styles.push(s);
            runs.push(Run { start: 0, end: text.len(), attrs: 0 });
            units.push(0..text.encode_utf16().count());
        }
        let direction = attrs.first().map_or(Direction::Natural, |a| a.paragraph.direction);
        let mut bytes = Vec::with_capacity(text.len() + 1);
        for (i, c) in text.char_indices() {
            bytes.extend(std::iter::repeat_n(i as u32, c.len_utf16()));
        }
        bytes.push(text.len() as u32);
        let utf16_len = bytes.len() - 1;
        Styled { text: text.to_owned(), attrs, styles, runs, units, utf16_len, bytes, direction }
    })
}

impl Styled {
    /// The part of the text over UTF-16 units `range`: its text and runs
    /// (the attributes are shared), and the first unit's byte.
    pub(crate) fn slice(&self, range: Range<usize>) -> (String, Vec<Run>) {
        let (start, end) = (self.byte_at(range.start), self.byte_at(range.end));
        let text = self.text[start..end].to_owned();
        let first = self.runs.partition_point(|r| r.end <= start);
        let mut runs: Vec<Run> = self.runs[first..]
            .iter()
            .take_while(|r| r.start < end)
            .filter(|r| r.end > start)
            .map(|r| Run { start: r.start.max(start) - start, end: r.end.min(end) - start, attrs: r.attrs })
            .collect();
        if runs.is_empty() {
            let attrs = self.runs.iter().find(|r| r.start >= start).or(self.runs.last()).map_or(0, |r| r.attrs);
            runs.push(Run { start: 0, end: text.len(), attrs });
        }
        (text, runs)
    }

    /// The byte of the text where UTF-16 unit `unit` starts (its
    /// character's, inside a pair; the end past it).
    pub(crate) fn byte_at(&self, unit: usize) -> usize {
        self.bytes[unit.min(self.utf16_len)] as usize
    }

    /// The UTF-16 units of the text's characters from `from` to `end`, one
    /// per character (a pair's second unit left out).
    pub(crate) fn char_starts(&self, from: usize, end: usize) -> Vec<usize> {
        let end = end.min(self.utf16_len);
        (from..end).filter(|&u| u == 0 || self.bytes[u] != self.bytes[u - 1]).collect()
    }

    /// Where the paragraph that UTF-16 unit `start` is in ends: after its
    /// separator ("\r\n" is one), or at the text's end.
    pub(crate) fn paragraph_end(&self, start: usize) -> usize {
        let from = self.byte_at(start);
        let rest = &self.text[from..];
        let mut chars = rest.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if matches!(c, '\n' | '\r' | '\u{2029}' | '\u{2028}' | '\u{85}') {
                let mut end = from + i + c.len_utf8();
                if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
                    end += 1;
                }
                return self.unit_at(end);
            }
        }
        self.utf16_len
    }

    /// The UTF-16 unit of the character starting at `byte`.
    fn unit_at(&self, byte: usize) -> usize {
        self.bytes.partition_point(|&b| (b as usize) < byte)
    }
}
