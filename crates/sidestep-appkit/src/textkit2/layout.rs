//! An element's text laid out as a layout fragment holds it: through the
//! same paragraph engine as TextKit 1 (`text::lines`), a paragraph of the
//! element at a time, stacked as TextKit 2 stacks them.
//!
//! Measured on macOS (`conformance/tests/textkit2.rs`), and the model here:
//!
//! - An element's text may hold several paragraphs (a content storage
//!   subclass's element grouping them, or a delegate's substitute); each
//!   separator inside it starts a paragraph of its own, with its own style.
//! - Line spacing goes above every line but the document's first, so
//!   within a paragraph it is between lines, and a paragraph after another
//!   starts that far down plus its spacing before; the first element of
//!   the document starts at its first line's top.
//! - A paragraph's spacing after it follows its last line, but for the
//!   document's last paragraph.
//! - The document's last element, when its text ends in a separator, ends
//!   in an empty line, spaced as a paragraph of its own (the extra line
//!   fragment, inside the fragment).
//! - The fragment's frame is as wide as its lines reach, from the leftmost
//!   line's start (indents and alignment move it) to the furthest line's
//!   end, trailing spaces included; its lines' typographic bounds are
//!   relative to that frame, and a line's glyph origin is its baseline.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSAttributedString, NSRange, NSString};

use crate::text::layout::Attrs;
use crate::text::lines::{self, Container, ParagraphLines, Span, Styled};
use crate::textkit::attrs::Dict;

/// Attributes resolved for the text engine, by dictionary (dictionaries a
/// storage hands out are interned, so each is worked out once). Spans index
/// `attrs`, which only grows until it is cleared between layouts.
#[derive(Default)]
pub(crate) struct Resolved {
    by_dict: HashMap<usize, (Retained<Dict>, u32)>,
    pub attrs: Vec<Attrs>,
}

/// Dictionaries remembered before the table is emptied.
const RESOLVED_MAX: usize = 4096;

impl Resolved {
    /// The index of `dict`'s attributes (`None`: the defaults).
    pub fn index(&mut self, dict: Option<&Dict>) -> u32 {
        let key = dict.map_or(0, |d| d as *const Dict as usize);
        if let Some((_, i)) = self.by_dict.get(&key) {
            return *i;
        }
        let i = self.attrs.len() as u32;
        self.attrs.push(crate::string_drawing::attrs_of(dict));
        if let Some(d) = dict {
            self.by_dict.insert(key, (objc2::Message::retain(d), i));
        } else {
            self.by_dict.insert(0, (objc2_foundation::NSDictionary::new(), i));
        }
        i
    }

    /// Forget what was resolved, when there is much of it (colors resolve
    /// for the appearance of the time, too).
    pub fn trim(&mut self) {
        if self.attrs.len() > RESOLVED_MAX {
            self.by_dict.clear();
            self.attrs.clear();
        }
    }
}

/// An element's text with its attribute runs over UTF-16 units, the
/// attributes indexing a [`Resolved`].
pub(crate) struct Text {
    pub text: String,
    pub spans: Vec<Span>,
}

/// The text of `range` of a Sidestep text storage, read from its paragraph
/// tree (attributes fixed first where the storage put that off).
pub(crate) fn storage_text(
    storage: &objc2_app_kit::NSTextStorage,
    range: Range<usize>,
    resolved: &mut Resolved,
) -> Option<Text> {
    crate::textkit::text_storage::ensure_fixed(storage, range.clone());
    let obj: &AnyObject = storage;
    let iv = crate::textkit::text_storage::native(obj)?;
    let text = iv.text();
    let range = range.start.min(text.len())..range.end.min(text.len());
    let s = text.text(range.clone());
    let mut ids = Vec::new();
    text.for_each_run(range.clone(), |r, id| ids.push((r, id)));
    drop(text);
    let table = iv.attrs().borrow();
    let spans = ids
        .into_iter()
        .map(|(r, id)| Span {
            start: (r.start - range.start) as u32,
            end: (r.end - range.start) as u32,
            attrs: crate::attachment::at_index(r.start, || resolved.index(Some(table.dict(id)))),
        })
        .collect();
    Some(Text { text: s, spans })
}

/// The text of an attributed string, read through its methods; its first
/// character is at `start` in the document (for what attachments are told).
pub(crate) fn attributed_text(a: &NSAttributedString, start: usize, resolved: &mut Resolved) -> Text {
    let string: Retained<NSString> = a.string();
    let len = string.length();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < len {
        let mut r = NSRange::new(0, 0);
        // SAFETY: an index inside the string, and a valid out-parameter.
        let d: Retained<Dict> = unsafe { msg_send![a, attributesAtIndex: i, effectiveRange: &mut r] };
        let end = (r.location + r.length).min(len).max(i + 1);
        let attrs = crate::attachment::at_index(start + i, || resolved.index(Some(&d)));
        spans.push(Span { start: i as u32, end: end as u32, attrs });
        i = end;
    }
    Text { text: string.to_string(), spans }
}

/// What layout needs of the container and the layout manager.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    /// The width lines take: the container's less its padding at each end
    /// (infinite for none).
    pub width: f32,
    pub padding: f64,
    pub font_leading: bool,
}

/// A paragraph of an element, laid out.
#[derive(Clone, Debug)]
pub(crate) struct Para {
    /// Its first UTF-16 unit in the element's text.
    pub start: u32,
    /// Its first line's top, from the fragment's top.
    pub top: f32,
    pub lines: Arc<ParagraphLines>,
    /// The spacing above its first line and below its last that belong to
    /// them (for finding lines by height).
    pub lead: f32,
    pub trail: f32,
}

/// An element laid out.
#[derive(Clone, Debug)]
pub(crate) struct Laid {
    pub paras: Vec<Para>,
    /// The leftmost line's start and the furthest line's end, from the
    /// container's padding edge.
    pub min_x: f32,
    pub max_x: f32,
    /// The fragment's height.
    pub height: f32,
    /// The UTF-16 units laid out.
    pub len: u32,
}

impl Laid {
    /// Every line, with its paragraph.
    pub fn lines(&self) -> impl Iterator<Item = (&Para, usize)> {
        self.paras.iter().flat_map(|p| (0..p.lines.lines.len()).map(move |i| (p, i)))
    }

    /// The line holding `index` (from the element's start); with
    /// `upstream`, an index where a wrapped line ends is that line's.
    pub fn line_at(&self, index: u32, upstream: bool) -> Option<(&Para, usize)> {
        let p = self.paras.partition_point(|p| p.start <= index).saturating_sub(1);
        let para = self.paras.get(p)?;
        let i = para.lines.line_at(index - para.start.min(index), upstream)?;
        Some((para, i))
    }
}

/// Lay out `text` (attributes from `attrs`): `first` if it is the
/// document's first element (nothing above it), `last` if its last, when
/// `open` says its text ends in a separator (so an empty line follows,
/// with `extra`'s attributes).
pub(crate) fn lay_out(
    text: &Text,
    attrs: &[Attrs],
    g: &Geometry,
    (first, last, open): (bool, bool, bool),
    extra: Option<&Attrs>,
) -> Laid {
    let container = Container { width: g.width, font_leading: g.font_leading, ..Container::UNBOUNDED };
    let styled = Styled { text: &text.text, attrs, spans: &text.spans };
    let mut paras: Vec<Para> = Vec::new();
    let mut byte = 0usize;
    let mut base = 0u32;
    loop {
        let lines = lines::lay_out_paragraph_in(styled, byte, base, &container);
        let consumed = lines.bytes;
        let len = lines.len;
        push(&mut paras, base, lines, first);
        byte += consumed;
        base += len;
        if consumed == 0 || byte >= text.text.len() {
            break;
        }
    }
    let ends_open = last && open;
    if ends_open && !text.text.is_empty() {
        let fallback;
        let attrs = match extra {
            Some(a) => a,
            None => {
                fallback = crate::string_drawing::attrs_of(None);
                &fallback
            }
        };
        let empty = Styled { text: "", attrs: std::slice::from_ref(attrs), spans: &[] };
        let lines = lines::lay_out_paragraph(empty, &container, 0);
        push(&mut paras, base, lines, first);
    }
    // The spacing after the last paragraph, unless it ends the document.
    let tail = match paras.last() {
        Some(p) if !last => p.lines.spacing.after,
        _ => 0.0,
    };
    if let Some(p) = paras.last_mut() {
        p.trail = tail;
    }
    let height = paras.last().map_or(0.0, |p| p.top + p.lines.height()) + tail;
    let (mut min_x, mut max_x) = (f32::INFINITY, f32::NEG_INFINITY);
    for p in &paras {
        for l in &p.lines.lines {
            min_x = min_x.min(l.x);
            max_x = max_x.max(l.x + l.width);
        }
    }
    if min_x > max_x {
        (min_x, max_x) = (0.0, 0.0);
    }
    Laid { paras, min_x, max_x, height, len: base }
}

/// Stack a paragraph under the ones laid out so far.
fn push(paras: &mut Vec<Para>, start: u32, lines: ParagraphLines, first: bool) {
    let s = lines.spacing;
    let (top, lead) = match paras.last_mut() {
        Some(prev) => {
            let after = prev.lines.spacing.after;
            prev.trail = after;
            let bottom = prev.top + prev.lines.height();
            (bottom + after + s.before + s.line, s.before + s.line)
        }
        None if first => (0.0, 0.0),
        None => (s.before + s.line, s.before + s.line),
    };
    paras.push(Para { start, top, lines: Arc::new(lines), lead, trail: 0.0 });
}

/// Whether `text` ends in a paragraph separator.
pub(crate) fn ends_in_separator(text: &str) -> bool {
    matches!(text.chars().next_back(), Some('\n' | '\r' | '\u{2029}'))
}
