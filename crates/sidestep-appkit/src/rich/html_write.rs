//! Writing a [`Doc`] as HTML, structured as AppKit's HTML writer
//! structures it on macOS (measured there; the bytes differ): an HTML 4.01
//! document in UTF-8 with a `<style>` sheet of classes, a `<p class="pN">`
//! per paragraph and `<span class="sN">` for runs that differ from their
//! paragraph.
//!
//! - A paragraph's class has its margins (spacing before, the trailing
//!   indent, spacing after, the head indent), `text-indent`, `text-align`,
//!   `line-height` (a minimum line height) and, when every run of the
//!   paragraph shares them, its font and color.
//! - A run in its paragraph's font but bolder or slanted is in `<b>` or
//!   `<i>`; a run in another font or size, color, background, underline or
//!   strikethrough, baseline offset or kerning gets a span class; super-
//!   and subscripts are `<sup>` and `<sub>`, links `<a href>`.
//! - Tabs are in `Apple-tab-span` spans and runs of spaces in
//!   `Apple-converted-space` ones, with no-break spaces, so browsers keep
//!   them; U+2028 is `<br>`, and an empty paragraph `<br>` alone.
//! - Fonts are named by family (`'Helvetica Neue'`), sizes in pixels (which
//!   are points here); colors as `#rrggbb`, or `rgba()` when not opaque.
//! - The document's title is `<title>`.

use std::collections::HashMap;
use std::fmt::Write;

use super::model::{Align, CharStyle, Color, Direction, Doc, Font, Generic, ParaStyle};

/// The AppKit version the `CocoaVersion` meta tag names, as a Cocoa HTML
/// writer's does (macOS 26's).
pub(crate) const COCOA_VERSION: &str = "2685.7";

/// `doc` as HTML (UTF-8).
pub(crate) fn write(doc: &Doc) -> Vec<u8> {
    let mut p_classes = Classes::default();
    let mut s_classes = Classes::default();
    let mut tabs = false;
    let mut body = String::new();
    let runs: Vec<_> = doc.run_ranges().collect();
    // The first run that ends after the paragraph's start: paragraphs and
    // runs are in order, so each is passed once.
    let mut run = 0;
    let empty_para = ParaStyle::default();
    if !doc.text.is_empty() {
        for (range, para) in doc.para_ranges() {
            let end = body_end(&doc.text, range.clone());
            let para = para.unwrap_or(&empty_para);
            while run < runs.len() && runs[run].0.end <= range.start {
                run += 1;
            }
            // The runs in this paragraph (the separator's run for an empty
            // one).
            let mut pieces: Vec<(&str, &CharStyle)> = Vec::new();
            for (r, style) in runs[run..].iter().take_while(|(r, _)| r.start < end) {
                let (s, e) = (r.start.max(range.start), r.end.min(end));
                if s < e {
                    pieces.push((&doc.text[s..e], style));
                }
            }
            let first_style = runs.get(run).map(|(_, s)| *s);
            let base = paragraph_base(&pieces, first_style);
            let css = paragraph_css(para, &base);
            let class = p_classes.index(css);
            let dir = if para.direction == Direction::RightToLeft { " dir=\"rtl\"" } else { "" };
            let _ = write!(body, "<p class=\"p{class}\"{dir}>");
            if pieces.is_empty() {
                body.push_str("<br>");
            }
            let mut link: Option<&str> = None;
            let last = pieces.len().saturating_sub(1);
            for (i, (text, style)) in pieces.iter().enumerate() {
                if style.link.as_deref() != link {
                    if link.is_some() {
                        body.push_str("</a>");
                    }
                    if let Some(l) = &style.link {
                        let _ = write!(body, "<a href=\"{}\">", escape(l, true));
                    }
                    link = style.link.as_deref();
                }
                let span = span_css(style, &base);
                let (open, close) = inline_tags(style, &base);
                let spanned = !span.is_empty();
                if spanned {
                    let class = s_classes.index(span);
                    let _ = write!(body, "<span class=\"s{class}\">");
                }
                body.push_str(&open);
                tabs |= text_html(&mut body, text, i == 0, i == last);
                body.push_str(&close);
                if spanned {
                    body.push_str("</span>");
                }
            }
            if link.is_some() {
                body.push_str("</a>");
            }
            body.push_str("</p>\n");
        }
    }
    let mut out = String::new();
    out.push_str("<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"http://www.w3.org/TR/html4/strict.dtd\">\n<html>\n<head>\n");
    out.push_str("<meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\">\n");
    out.push_str("<meta http-equiv=\"Content-Style-Type\" content=\"text/css\">\n");
    let _ = writeln!(out, "<title>{}</title>", escape(doc.attrs.title.as_deref().unwrap_or(""), false));
    out.push_str("<meta name=\"Generator\" content=\"Cocoa HTML Writer\">\n");
    let _ = writeln!(out, "<meta name=\"CocoaVersion\" content=\"{COCOA_VERSION}\">");
    out.push_str("<style type=\"text/css\">\n");
    for (i, css) in p_classes.list.iter().enumerate() {
        let _ = writeln!(out, "p.p{} {{{css}}}", i + 1);
    }
    for (i, css) in s_classes.list.iter().enumerate() {
        let _ = writeln!(out, "span.s{} {{{css}}}", i + 1);
    }
    if tabs {
        out.push_str("span.Apple-tab-span {white-space:pre}\n");
    }
    out.push_str("</style>\n</head>\n<body>\n");
    out.push_str(&body);
    out.push_str("</body>\n</html>\n");
    out.into_bytes()
}

/// A style sheet's classes, in the order they were first used.
#[derive(Default)]
struct Classes {
    list: Vec<String>,
    numbers: HashMap<String, usize>,
}

impl Classes {
    /// The 1-based number of the class `css`, added if new.
    fn index(&mut self, css: String) -> usize {
        if let Some(&n) = self.numbers.get(&css) {
            return n;
        }
        self.list.push(css.clone());
        self.numbers.insert(css, self.list.len());
        self.list.len()
    }
}

/// Where the text of the paragraph `range` ends: before its separator.
fn body_end(text: &str, range: std::ops::Range<usize>) -> usize {
    let t = &text[range.clone()];
    let sep = if t.ends_with("\r\n") {
        2
    } else if t.ends_with(['\n', '\r']) {
        1
    } else if t.ends_with('\u{2029}') {
        3
    } else {
        0
    };
    range.end - sep
}

/// What a paragraph's class gives its runs: its first run's font (as a
/// regular face: bold and italic go in tags), and a color when every run
/// has it.
struct Base {
    font: Option<Font>,
    color: Option<Option<Color>>,
}

fn paragraph_base(pieces: &[(&str, &CharStyle)], first: Option<&CharStyle>) -> Base {
    let plain = |f: &Font| Font { bold: false, italic: false, ..f.clone() };
    let first = pieces.first().map(|(_, s)| *s).or(first);
    let font = first.and_then(|s| s.font.as_ref()).map(plain);
    let color = first.map(|s| s.color.clone());
    let same_color = pieces.iter().all(|(_, s)| Some(s.color.clone()) == color);
    Base { font, color: color.filter(|_| same_color) }
}

fn px(v: f64) -> String {
    format!("{v:.1}px")
}

/// A font's family as CSS names it: quoted if it has spaces.
fn family(f: &Font) -> String {
    let name = if f.family.is_empty() { f.names.first().cloned().unwrap_or_default() } else { f.family.clone() };
    if name.contains(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        format!("'{}'", name.replace('\'', "\\'"))
    } else {
        name
    }
}

/// The `font` shorthand for `f`, with its bold and italic when asked.
fn font_css(f: &Font, traits: bool) -> String {
    let mut s = String::new();
    if traits && f.italic {
        s.push_str("italic ");
    }
    if traits && f.bold {
        s.push_str("bold ");
    }
    let _ = write!(s, "{} {}", px(f.size), family(f));
    s
}

fn color_css(c: &Color) -> String {
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let [r, g, b, a] = c.srgb;
    if a >= 1.0 {
        format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
    } else {
        format!("rgba({}, {}, {}, {})", byte(r), byte(g), byte(b), (a * 1000.0).round() / 1000.0)
    }
}

fn paragraph_css(p: &ParaStyle, base: &Base) -> String {
    let right = if p.tail_indent < 0.0 { -p.tail_indent } else { 0.0 };
    let mut css = format!(
        "margin: {} {} {} {}",
        px(p.paragraph_spacing_before),
        px(right),
        px(p.paragraph_spacing),
        px(p.head_indent)
    );
    if p.first_line_head_indent != p.head_indent {
        let _ = write!(css, "; text-indent: {}", px(p.first_line_head_indent - p.head_indent));
    }
    let align = match p.alignment {
        Align::Center => Some("center"),
        Align::Right => Some("right"),
        Align::Justified => Some("justify"),
        Align::Left if p.direction == Direction::RightToLeft => Some("left"),
        _ => None,
    };
    if let Some(a) = align {
        let _ = write!(css, "; text-align: {a}");
    }
    if p.minimum_line_height > 0.0 {
        let _ = write!(css, "; line-height: {}", px(p.minimum_line_height));
    }
    if let Some(f) = &base.font {
        let _ = write!(css, "; font: {}", font_css(f, false));
    }
    if let Some(Some(c)) = &base.color {
        let _ = write!(css, "; color: {}", color_css(c));
    }
    css
}

/// A run's span class: what it has that its paragraph doesn't. Text
/// without a font is in Helvetica 12, which AppKit writes for it.
fn span_css(s: &CharStyle, base: &Base) -> String {
    let mut parts: Vec<String> = Vec::new();
    let default_font;
    let f = match &s.font {
        Some(f) => f,
        None => {
            default_font = Font::named("Helvetica", Generic::Sans, 12.0);
            &default_font
        }
    };
    let same = base.font.as_ref().is_some_and(|b| b.names == f.names || (b.family == f.family && !f.family.is_empty()))
        && base.font.as_ref().is_some_and(|b| b.size == f.size);
    if !same {
        parts.push(format!("font: {}", font_css(f, true)));
    }
    if base.color.is_none()
        && let Some(c) = &s.color
    {
        parts.push(format!("color: {}", color_css(c)));
    }
    if let Some(c) = &s.background {
        parts.push(format!("background-color: {}", color_css(c)));
    }
    let mut decoration = Vec::new();
    if s.underline != 0 {
        decoration.push("underline");
    }
    if s.strikethrough != 0 {
        decoration.push("line-through");
    }
    if !decoration.is_empty() {
        parts.push(format!("text-decoration: {}", decoration.join(" ")));
    }
    if s.baseline_offset != 0.0 {
        parts.push(format!("vertical-align: {}", px(s.baseline_offset)));
    }
    if let Some(k) = s.kern
        && k != 0.0
    {
        parts.push(format!("letter-spacing: {}", px(k)));
    }
    parts.join("; ")
}

/// The tags a run's text goes in: `<b>`, `<i>` for a bolder or slanted
/// face of the paragraph's font, `<sup>`, `<sub>`.
fn inline_tags(s: &CharStyle, base: &Base) -> (String, String) {
    let (mut open, mut close) = (String::new(), String::new());
    let mut tag = |name: &str| {
        let _ = write!(open, "<{name}>");
        close.insert_str(0, &format!("</{name}>"));
    };
    if let (Some(f), Some(b)) = (&s.font, &base.font)
        && (b.names == f.names || b.family == f.family && !f.family.is_empty())
        && b.size == f.size
    {
        if f.bold {
            tag("b");
        }
        if f.italic {
            tag("i");
        }
    }
    if s.superscript > 0 {
        tag("sup");
    } else if s.superscript < 0 {
        tag("sub");
    }
    (open, close)
}

/// `text` as HTML: escaped, tabs and runs of spaces kept (and a space at
/// the paragraph's start or end). True if it had a tab.
fn text_html(out: &mut String, text: &str, para_start: bool, para_end: bool) -> bool {
    let mut tabs = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\t' => {
                out.push_str("<span class=\"Apple-tab-span\">\t</span>");
                tabs = true;
            }
            ' ' => {
                let n = chars[i..].iter().take_while(|&&c| c == ' ').count();
                let at_edge = (i == 0 && para_start) || (i + n == chars.len() && para_end);
                if n == 1 && !at_edge {
                    out.push(' ');
                } else {
                    // Browsers collapse spaces: all but the last are no-break
                    // spaces.
                    out.push_str("<span class=\"Apple-converted-space\">");
                    for j in 0..n {
                        out.push(if j + 1 < n || (i + n == chars.len() && para_end) { '\u{A0}' } else { ' ' });
                    }
                    out.push_str("</span>");
                }
                i += n;
                continue;
            }
            '\u{2028}' => out.push_str("<br>"),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
        i += 1;
    }
    tabs
}

/// Text escaped for HTML: `&`, `<`, `>`, and in attributes `"`.
fn escape(text: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::html_read;
    use super::super::model::Builder;
    use super::*;

    fn helvetica(bold: bool, italic: bool) -> Font {
        Font {
            names: vec![
                match (bold, italic) {
                    (false, false) => "Helvetica",
                    (true, false) => "Helvetica-Bold",
                    (false, true) => "Helvetica-Oblique",
                    (true, true) => "Helvetica-BoldOblique",
                }
                .into(),
            ],
            family: "Helvetica".into(),
            generic: Generic::Sans,
            size: 12.0,
            bold,
            italic,
        }
    }

    fn plain() -> CharStyle {
        CharStyle { font: Some(helvetica(false, false)), ..CharStyle::default() }
    }

    fn html(doc: &Doc) -> String {
        String::from_utf8(write(doc)).unwrap()
    }

    #[test]
    fn documents_are_structured_as_appkit_writes_them() {
        let mut b = Builder::new();
        b.push("Normal ", &plain());
        b.push("Bold", &CharStyle { font: Some(helvetica(true, false)), ..plain() });
        b.push(" ", &plain());
        b.push("Italic", &CharStyle { font: Some(helvetica(false, true)), ..plain() });
        b.push(" ", &plain());
        b.push(
            "Big",
            &CharStyle {
                font: Some(Font {
                    size: 18.5,
                    names: vec!["Times-Roman".into()],
                    family: "Times".into(),
                    ..helvetica(false, false)
                }),
                ..plain()
            },
        );
        let out = html(&b.finish(None));
        assert!(out.starts_with("<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01//EN\""), "{out}");
        assert!(out.contains("<meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\">"), "{out}");
        assert!(
            out.contains(
                "p.p1 {margin: 0.0px 0.0px 0.0px 0.0px; font: 12.0px Helvetica}\nspan.s1 {font: 18.5px Times}"
            ),
            "{out}"
        );
        assert!(
            out.contains("<p class=\"p1\">Normal <b>Bold</b> <i>Italic</i> <span class=\"s1\">Big</span></p>"),
            "{out}"
        );
    }

    /// Text without a font is in a Helvetica 12 span, as AppKit writes it.
    #[test]
    fn text_without_a_font() {
        let mut b = Builder::new();
        b.push("a\tb", &CharStyle::default());
        let out = html(&b.finish(None));
        assert!(out.contains("p.p1 {margin: 0.0px 0.0px 0.0px 0.0px}\nspan.s1 {font: 12.0px Helvetica}\n"), "{out}");
        assert!(
            out.contains("<p class=\"p1\"><span class=\"s1\">a<span class=\"Apple-tab-span\">\t</span>b</span></p>"),
            "{out}"
        );
    }

    #[test]
    fn one_font_paragraphs_carry_it() {
        let mut b = Builder::new();
        b.push("Normal ", &plain());
        b.push("Bold", &CharStyle { font: Some(helvetica(true, false)), ..plain() });
        b.push(" ", &plain());
        b.push("red", &CharStyle { color: Some(Color::srgb(1.0, 0.0, 0.0, 1.0)), ..plain() });
        b.push(" <&> ", &CharStyle { underline: 1, strikethrough: 1, ..plain() });
        b.push("sup", &CharStyle { superscript: 1, ..plain() });
        b.push("link", &CharStyle { link: Some("https://example.com/?a=1&b=\"2\"".into()), ..plain() });
        b.push("\ttab  two  spaces", &plain());
        let out = html(&b.finish(None));
        assert!(out.contains("p.p1 {margin: 0.0px 0.0px 0.0px 0.0px; font: 12.0px Helvetica}"), "{out}");
        assert!(out.contains("<p class=\"p1\">Normal <b>Bold</b> <span class=\"s1\">red</span><span class=\"s2\"> &lt;&amp;&gt; </span><sup>sup</sup><a href=\"https://example.com/?a=1&amp;b=&quot;2&quot;\">link</a><span class=\"Apple-tab-span\">\t</span>tab<span class=\"Apple-converted-space\">\u{a0} </span>two<span class=\"Apple-converted-space\">\u{a0} </span>spaces</p>"), "{out}");
        assert!(out.contains("span.s1 {color: #ff0000}\nspan.s2 {text-decoration: underline line-through}\nspan.Apple-tab-span {white-space:pre}"), "{out}");
    }

    #[test]
    fn paragraphs_and_their_styles() {
        let mut b = Builder::new();
        let centered = ParaStyle {
            alignment: Align::Center,
            paragraph_spacing: 10.0,
            paragraph_spacing_before: 5.0,
            ..ParaStyle::default()
        };
        let indented =
            ParaStyle { first_line_head_indent: 36.0, head_indent: 18.0, tail_indent: -20.0, ..ParaStyle::default() };
        b.push("Centered", &plain());
        b.end_paragraph("\n", &plain(), Some(&centered));
        b.end_paragraph("\n", &plain(), None);
        b.push("Indented\u{2028}line", &plain());
        b.end_paragraph("\n", &plain(), Some(&indented));
        let out = html(&b.finish(None));
        assert!(
            out.contains("p.p1 {margin: 5.0px 0.0px 10.0px 0.0px; text-align: center; font: 12.0px Helvetica}"),
            "{out}"
        );
        assert!(
            out.contains("p.p3 {margin: 0.0px 20.0px 0.0px 18.0px; text-indent: 18.0px; font: 12.0px Helvetica}"),
            "{out}"
        );
        assert!(
            out.contains(
                "<p class=\"p1\">Centered</p>\n<p class=\"p2\"><br></p>\n<p class=\"p3\">Indented<br>line</p>\n"
            ),
            "{out}"
        );
    }

    #[test]
    fn what_is_written_reads_back() {
        let mut b = Builder::new();
        let red = Color::srgb(1.0, 0.0, 0.0, 1.0);
        b.push("Normal ", &plain());
        b.push("Bold", &CharStyle { font: Some(helvetica(true, false)), ..plain() });
        b.push(" red", &CharStyle { color: Some(red), ..plain() });
        b.push(" under", &CharStyle { underline: 1, ..plain() });
        b.push(" link", &CharStyle { link: Some("https://example.com/a".into()), ..plain() });
        b.end_paragraph("\n", &plain(), Some(&ParaStyle { alignment: Align::Center, ..ParaStyle::default() }));
        b.push("\ttab and\u{a0}space  two", &plain());
        let doc = b.finish(None);
        let back = html_read::read(
            &html(&doc),
            &html_read::Options { encoding: Some(html_read::Encoding::Utf8), ..html_read::Options::default() },
        );
        // (Runs of spaces come back as spaces, no-break spaces as themselves.)
        assert_eq!(back.text, "Normal Bold red under link\n\ttab and\u{a0}space  two\n");
        let r: Vec<_> = back.run_ranges().map(|(r, s)| (&back.text[r], s.clone())).collect();
        let get = |t: &str| {
            r.iter().find(|(x, _)| *x == t).map(|(_, s)| s.clone()).unwrap_or_else(|| panic!("{t:?} in {r:?}"))
        };
        assert!(get("Bold").font.unwrap().bold);
        assert_eq!(get("Normal ").font.unwrap().names, ["Helvetica"]);
        assert_eq!(get(" red").color.unwrap().srgb, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(get(" under").underline, 1);
        assert_eq!(get(" link").link.as_deref(), Some("https://example.com/a"));
        assert_eq!(back.paras[0].1.as_ref().unwrap().alignment, Align::Center);
    }
}
