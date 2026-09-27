//! Reading HTML into a [`Doc`], as macOS reads it into an attributed string
//! (through WebKit there; here without a browser engine). What it makes of
//! the markup browsers and editors put on the pasteboard was measured on
//! macOS and is followed where a program can see it:
//!
//! - Text is Times 12 in black; CSS pixels are points (`pt` are 4/3 as
//!   big); `monospace` alone is Courier 13; `<b>`, `<strong>`, `<th>` and
//!   `font-weight` 600 and up are bold, `<i>`, `<em>` and `<cite>` italic,
//!   `<u>` and `<ins>` underlined, `<s>`, `<strike>` and `<del>` struck
//!   through; `<code>`, `<tt>`, `<kbd>`, `<samp>` and `<pre>` monospaced;
//!   `<sup>` and `<sub>` are superscript and subscript in a smaller size;
//!   headings bold at 24, 18, 14, 12, 10 and 9 points with their header
//!   level; links blue (#0000EE) and underlined, with `NSLink` when their
//!   target is absolute or a base URL makes it so.
//! - Every paragraph has a paragraph style: left to right (or right to
//!   left with `dir="rtl"`), no tab stops and a 36-point default tab
//!   interval, the block's `text-align`, and its bottom margin as the
//!   spacing after (a `<p>`'s 1em; a heading's; none for `<div>`, `<pre>`
//!   and a list item's own). Blocks' left margins and paddings add up as the head
//!   indent (`<blockquote>` 40 points and as much on the right), and
//!   `text-indent` goes on the first line.
//! - Blocks end paragraphs, the last one too (`<body>` and `<html>`
//!   aren't blocks: text after the last block has no paragraph end).
//!   `<br>` is U+2028 in a paragraph, a heading or a list item and ends the
//!   paragraph elsewhere; one that ends its block shows nothing, unless the
//!   block holds nothing else (an empty line), but outside any block every
//!   one does. White space collapses outside `<pre>` (and `white-space:
//!   pre`), and a newline right after `<pre>` is dropped. The no-break
//!   spaces of an `Apple-converted-space` span are spaces.
//! - List items start with their marker between tabs (`\t•\t`, `\t◦\t`
//!   and `\t▪\t` by depth, `\t1\t` in ordered lists), in the style of
//!   the item's first text (without its link, underline or
//!   strikethrough), left-aligned unless a block says, indented 36
//!   points a level (not by margins) with tab stops at the marker and
//!   the text; the first line starts at the page's margin. Other blocks
//!   in an item (`<p>`, `<div>`, headings) go on in the item's
//!   paragraph, a line (U+2028) each, and the paragraph takes the style
//!   around its first text (a `<p>`'s spacing after, a heading's level);
//!   nested lists, tables and preformatted text end it, and the item's
//!   text after them, each table cell and each preformatted line get its
//!   marker again. An empty item is its marker alone; one holding only a
//!   nested list has none. Letters go round the alphabet (the 27th is
//!   `a` again), Roman numerals stop at 3999; a list's `start` is read
//!   as HTML reads integers, and an item's `value` is ignored.
//! - Table cells are paragraphs of their own.
//! - Inline `style` attributes and `<style>` sheets with simple selectors
//!   apply (see `css`); so do `<font>`'s `color`, `size` (1 to 7: 9, 10,
//!   12, 14, 18, 24 and 37 points) and `face` and the `align` attribute.
//! - Opaque black text gets no color attribute, as the default.
//!
//! Pictures and embedded content are dropped (attachments come with
//! `NSTextAttachment`), as are `script`, `style`, `template` and the
//! document's head.
//!
//! Where Sidestep reads differently from macOS: block margins. A block's
//! left and right margins and left padding add up with its ancestors' as
//! the paragraph's indents, and its first line starts where the rest do
//! (plus `text-indent`); macOS takes only the innermost `<p>`'s or
//! `<blockquote>`'s margins (none of a `<div>`'s or a padding, and none
//! from a list's or an outer block's), and starts the first line at 0
//! unless `text-indent` is positive, so a `<blockquote>`'s first line
//! hangs out at the page's margin.

use super::css::{self, Sheet};
use super::html_lex::{Token, tokens};
use super::model::{Align, Builder, CharStyle, Color, Direction, Doc, Font, Generic, ParaStyle, Tab, TabKind};
use super::tables::CodePage;

/// How HTML's bytes are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Page(CodePage),
}

impl Encoding {
    /// The encoding a `charset` label names.
    pub fn from_label(label: &str) -> Option<Encoding> {
        let l = label.trim().to_ascii_lowercase();
        match l.as_str() {
            "utf-8" | "utf8" | "unicode-1-1-utf-8" => Some(Encoding::Utf8),
            "utf-16" | "utf-16le" | "unicode" => Some(Encoding::Utf16Le),
            "utf-16be" => Some(Encoding::Utf16Be),
            _ => CodePage::from_label(&l).map(Encoding::Page),
        }
    }
}

/// How to read HTML.
#[derive(Clone, Debug)]
pub(crate) struct Options {
    /// The encoding to read the bytes in, whatever they say.
    pub encoding: Option<Encoding>,
    /// The encoding when neither a byte order mark nor a `charset` says.
    pub fallback: Encoding,
    /// What relative links are relative to.
    pub base_url: Option<String>,
}

impl Default for Options {
    fn default() -> Options {
        // Windows-1252, as AppKit reads HTML that doesn't say.
        Options { encoding: None, fallback: Encoding::Page(CodePage::Cp1252), base_url: None }
    }
}

/// Whether `data` looks like an HTML document, as AppKit decides when it
/// isn't told the type: it starts, after white space, with `<html`,
/// `<!doctype html` or `<head` (any case).
pub(crate) fn is_html(data: &[u8]) -> bool {
    let start = data.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(data.len());
    let head: Vec<u8> = data[start..].iter().take(16).map(u8::to_ascii_lowercase).collect();
    head.starts_with(b"<html") || head.starts_with(b"<!doctype html") || head.starts_with(b"<head")
}

/// `data` as text: in the encoding its byte order mark gives, else the one
/// asked for, else the one a `<meta>` names, else the fallback.
pub(crate) fn decode_bytes(data: &[u8], options: &Options) -> String {
    let (encoding, data) = if let Some(rest) = data.strip_prefix(b"\xEF\xBB\xBF") {
        (Encoding::Utf8, rest)
    } else if let Some(rest) = data.strip_prefix(b"\xFF\xFE") {
        (Encoding::Utf16Le, rest)
    } else if let Some(rest) = data.strip_prefix(b"\xFE\xFF") {
        (Encoding::Utf16Be, rest)
    } else {
        (options.encoding.or_else(|| meta_charset(data)).unwrap_or(options.fallback), data)
    };
    match encoding {
        Encoding::Utf8 => String::from_utf8_lossy(data).into_owned(),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let units =
                data.as_chunks::<2>().0.iter().map(|&c| {
                    if encoding == Encoding::Utf16Le { u16::from_le_bytes(c) } else { u16::from_be_bytes(c) }
                });
            char::decode_utf16(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
        }
        Encoding::Page(page) => data.iter().map(|&b| page.decode(b)).collect(),
    }
}

/// The encoding a `<meta charset>` or `<meta http-equiv … content>` in the
/// first kilobyte names.
fn meta_charset(data: &[u8]) -> Option<Encoding> {
    let head = String::from_utf8_lossy(&data[..data.len().min(1024)]).to_ascii_lowercase();
    let mut rest = head.as_str();
    while let Some(i) = rest.find("<meta") {
        rest = &rest[i + 5..];
        let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
        if let Some(j) = tag.find("charset") {
            let value = tag[j + 7..].trim_start().strip_prefix('=')?.trim_start();
            let value = value.trim_start_matches(['"', '\'']);
            let end = value
                .find(|c: char| c == '"' || c == '\'' || c == ';' || c == '/' || c.is_whitespace())
                .unwrap_or(value.len());
            return Encoding::from_label(&value[..end]);
        }
    }
    None
}

/// Read HTML text.
pub(crate) fn read(html: &str, options: &Options) -> Doc {
    let mut r = Reader::new(options);
    r.run(&tokens(html));
    r.finish()
}

/// Read HTML bytes.
pub(crate) fn read_bytes(data: &[u8], options: &Options) -> Doc {
    read(&decode_bytes(data, options), options)
}

// Styles.

/// The computed style of an element, as far as rich text goes.
#[derive(Clone, Debug)]
struct Style {
    families: Vec<String>,
    size: f64,
    /// Whether some element set the size (or else `monospace` is 13).
    size_set: bool,
    bold: bool,
    italic: bool,
    color: Option<[f64; 4]>,
    background: Option<[f64; 4]>,
    underline: bool,
    strike: bool,
    superscript: i64,
    baseline: f64,
    link: Option<String>,
    pre: bool,
    align: Option<Align>,
    direction: Direction,
    header: i64,
    hidden: bool,
    /// In an `Apple-converted-space` span: its no-break spaces are spaces.
    converted_space: bool,
}

impl Style {
    fn root() -> Style {
        Style {
            families: vec!["Times".into()],
            size: 12.0,
            size_set: false,
            bold: false,
            italic: false,
            color: None,
            background: None,
            underline: false,
            strike: false,
            superscript: 0,
            baseline: 0.0,
            link: None,
            pre: false,
            align: None,
            direction: Direction::LeftToRight,
            header: 0,
            hidden: false,
            converted_space: false,
        }
    }

    fn char_style(&self) -> CharStyle {
        let mut names = Vec::new();
        let mut generic = Generic::Serif;
        for f in &self.families {
            match f.to_ascii_lowercase().as_str() {
                "serif" | "ui-serif" => generic = Generic::Serif,
                "sans-serif" | "cursive" | "fantasy" | "ui-sans-serif" | "ui-rounded" => generic = Generic::Sans,
                "monospace" | "ui-monospace" => generic = Generic::Mono,
                "system-ui" | "-apple-system" | "blinkmacsystemfont" => generic = Generic::System,
                _ => {
                    names.push(f.clone());
                    continue;
                }
            }
            break;
        }
        let monospace_alone = names.is_empty() && generic == Generic::Mono && self.families.len() == 1;
        let size = if monospace_alone && !self.size_set { 13.0 } else { self.size };
        let color = |c: [f64; 4]| Color::srgb(c[0], c[1], c[2], c[3]);
        CharStyle {
            font: Some(Font { names, family: String::new(), generic, size, bold: self.bold, italic: self.italic }),
            color: self.color.filter(|c| *c != [0.0, 0.0, 0.0, 1.0]).map(color),
            background: self.background.map(color),
            underline: i64::from(self.underline),
            strikethrough: i64::from(self.strike),
            link: self.link.clone(),
            baseline_offset: self.baseline,
            superscript: self.superscript,
            ..CharStyle::default()
        }
    }
}

/// A block's part in its paragraphs' style.
#[derive(Clone, Debug, Default)]
struct Block {
    left: f64,
    right: f64,
    indent: f64,
    after: f64,
    /// The bottom margin in ems of the block's own font size, unless CSS
    /// gives it.
    after_em: Option<f64>,
    /// A list item's depth.
    item: Option<usize>,
    /// A list item's marker, between its tabs, and where the text stood
    /// when the item began.
    marker: Option<(String, usize)>,
}

/// A line break (`<br>`) waiting to go in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Break {
    /// U+2028: the line ends, the paragraph goes on.
    Line,
    /// The paragraph ends.
    Paragraph,
    /// A block in a list item ended: U+2028 if text follows in the item,
    /// nothing if its paragraph ends first.
    Block,
}

/// An open list.
#[derive(Clone, Debug)]
struct List {
    kind: ListKind,
    next: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ListKind {
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    None,
}

impl ListKind {
    /// A `list-style-type` (any case).
    fn from_css(v: &str) -> Option<ListKind> {
        Some(match v.trim().to_ascii_lowercase().as_str() {
            "disc" => ListKind::Disc,
            "circle" => ListKind::Circle,
            "square" => ListKind::Square,
            "decimal" => ListKind::Decimal,
            "lower-alpha" | "lower-latin" => ListKind::LowerAlpha,
            "upper-alpha" | "upper-latin" => ListKind::UpperAlpha,
            "lower-roman" => ListKind::LowerRoman,
            "upper-roman" => ListKind::UpperRoman,
            "none" => ListKind::None,
            _ => return None,
        })
    }

    /// An `<ol>`'s `type`: its letters by case, or a `list-style-type`.
    fn from_type(v: &str) -> Option<ListKind> {
        Some(match v.trim() {
            "1" => ListKind::Decimal,
            "a" => ListKind::LowerAlpha,
            "A" => ListKind::UpperAlpha,
            "i" => ListKind::LowerRoman,
            "I" => ListKind::UpperRoman,
            other => return ListKind::from_css(other),
        })
    }

    /// Item `n`'s marker, as macOS numbers items: letters go round the
    /// alphabet (the 27th is `a` again, and counting down from `a` goes
    /// back through ASCII), Roman numerals stop at 3999 (none beyond, and
    /// decimal numbers from 0 down).
    fn marker(self, n: i64) -> String {
        let letter = |first: u8| {
            let offset = (n.wrapping_sub(1) % 26) as i8;
            char::from(first.wrapping_add_signed(offset)).to_string()
        };
        match self {
            ListKind::Disc => "•".into(),
            ListKind::Circle => "◦".into(),
            ListKind::Square => "▪".into(),
            ListKind::Decimal => n.to_string(),
            ListKind::LowerAlpha => letter(b'a'),
            ListKind::UpperAlpha => letter(b'A'),
            ListKind::LowerRoman | ListKind::UpperRoman if n <= 0 => n.to_string(),
            ListKind::LowerRoman => roman(n).to_lowercase(),
            ListKind::UpperRoman => roman(n),
            ListKind::None => String::new(),
        }
    }
}

/// `n` in Roman numerals, from 1 to 3999; nothing past that.
fn roman(mut n: i64) -> String {
    if n > 3999 {
        return String::new();
    }
    const R: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut s = String::new();
    for (v, r) in R {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

struct Frame {
    name: String,
    style: Style,
    block: Option<Block>,
    list: Option<List>,
    /// A block in a list item that keeps to the item's paragraph.
    merged: bool,
}

/// Elements that are blocks. (`<body>` and `<html>` aren't: text after
/// their last block is a paragraph without an end, as it is in AppKit.)
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "blockquote"
            | "pre"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "caption"
            | "address"
            | "article"
            | "aside"
            | "section"
            | "header"
            | "footer"
            | "nav"
            | "main"
            | "figure"
            | "figcaption"
            | "dl"
            | "dt"
            | "dd"
            | "form"
            | "fieldset"
            | "center"
            | "details"
            | "summary"
            | "hr"
            | "listing"
            | "plaintext"
            | "menu"
            | "dir"
    )
}

/// Blocks that end a list item's paragraph (lists, tables and preformatted
/// text): other blocks in an item go on in its paragraph, as macOS reads
/// them, with a line break (U+2028) where one ends.
fn ends_item_paragraph(name: &str) -> bool {
    matches!(
        name,
        "ul" | "ol"
            | "menu"
            | "dir"
            | "li"
            | "pre"
            | "listing"
            | "xmp"
            | "plaintext"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "caption"
    )
}

/// Elements that have no end tag.
fn is_void(name: &str) -> bool {
    matches!(
        name,
        "br" | "hr"
            | "img"
            | "meta"
            | "link"
            | "input"
            | "wbr"
            | "area"
            | "base"
            | "col"
            | "embed"
            | "param"
            | "source"
            | "track"
            | "keygen"
    )
}

/// Elements whose content isn't shown.
fn is_hidden(name: &str) -> bool {
    matches!(
        name,
        "head"
            | "script"
            | "style"
            | "template"
            | "noscript"
            | "iframe"
            | "object"
            | "svg"
            | "math"
            | "canvas"
            | "audio"
            | "video"
            | "select"
            | "datalist"
            | "title"
            | "noembed"
            | "noframes"
    )
}

/// Heading sizes and bottom margins (in ems), as the standard style sheet
/// has them (sizes as WebKit rounds them for 12-point text).
const HEADINGS: [(f64, f64); 6] = [(2.0, 0.67), (1.5, 0.83), (1.17, 1.0), (1.0, 1.33), (0.83, 1.67), (0.67, 2.33)];

struct Reader {
    out: Builder,
    stack: Vec<Frame>,
    sheet: Sheet,
    /// A collapsed space waiting for the next text, in its style.
    space: Option<CharStyle>,
    /// A newline right after `<pre>` is dropped.
    after_pre: bool,
    /// Nothing but a list marker is on the line yet: white space is
    /// dropped.
    fresh: bool,
    /// Line breaks waiting for text to follow them.
    breaks: Vec<Break>,
    /// A list item's marker, waiting for the item's first text (whose
    /// style it takes).
    marker: Option<String>,
    /// A list item's paragraph style: the one around its first text.
    item_style: Option<ParaStyle>,
    /// The style of the block a [`Break::Block`] ended.
    block_style: Option<CharStyle>,
    base: Option<String>,
    cocoa_version: Option<f64>,
}

impl Reader {
    fn new(options: &Options) -> Reader {
        Reader {
            out: Builder::new(),
            stack: vec![Frame {
                name: "#root".into(),
                style: Style::root(),
                block: Some(Block::default()),
                list: None,
                merged: false,
            }],
            sheet: Sheet::default(),
            space: None,
            after_pre: false,
            fresh: true,
            breaks: Vec::new(),
            marker: None,
            item_style: None,
            block_style: None,
            base: options.base_url.clone(),
            cocoa_version: None,
        }
    }

    fn style(&self) -> &Style {
        &self.stack.last().expect("the root").style
    }

    fn hidden(&self) -> bool {
        self.style().hidden
    }

    fn run(&mut self, tokens: &[Token]) {
        // Style sheets apply to the whole document, wherever they are.
        let mut in_style = false;
        for t in tokens {
            match t {
                Token::Start { name, .. } if name == "style" => in_style = true,
                Token::End { name } if name == "style" => in_style = false,
                Token::Text(text) if in_style => self.sheet.add(text),
                _ => {}
            }
        }
        for t in tokens {
            match t {
                Token::Start { name, closed, .. } => {
                    self.start(t);
                    if *closed && !is_void(name) {
                        self.end(name);
                    }
                }
                Token::End { name } => self.end(name),
                Token::Text(text) => self.text(text),
            }
        }
    }

    fn start(&mut self, token: &Token) {
        let Token::Start { name, .. } = token else { return };
        let name = name.as_str();
        match name {
            "meta" => {
                if token.attr("name").is_some_and(|n| n.eq_ignore_ascii_case("CocoaVersion")) {
                    self.cocoa_version = token.attr("content").and_then(|v| v.trim().parse().ok());
                }
                return;
            }
            "base" => {
                if let Some(href) = token.attr("href") {
                    self.base = Some(resolve(self.base.as_deref(), href).unwrap_or_else(|| href.to_owned()));
                }
                return;
            }
            "br" => {
                if !self.hidden() {
                    // Within a paragraph, a heading or a list item a break
                    // keeps its paragraph (U+2028); elsewhere it ends one.
                    // It goes in when more text follows in its block.
                    let within = self.stack.iter().rev().find(|f| f.block.is_some()).is_some_and(|f| {
                        matches!(f.name.as_str(), "p" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
                    });
                    self.space = None;
                    self.breaks.push(if within { Break::Line } else { Break::Paragraph });
                    self.fresh = true;
                }
                return;
            }
            "hr" => {
                if !self.hidden() {
                    self.drop_block_break();
                    self.flush_breaks();
                    self.end_paragraph(false);
                    self.end_paragraph(true);
                }
                return;
            }
            // A paragraph ends at a new one, where an HTML document leaves
            // the first open.
            "p" if self.stack.last().is_some_and(|f| f.name == "p") => self.end("p"),
            "li" if self
                .stack
                .iter()
                .rev()
                .take_while(|f| f.name != "ul" && f.name != "ol")
                .any(|f| f.name == "li") =>
            {
                self.end("li")
            }
            _ if is_void(name) => return,
            _ => {}
        }
        let parent = self.style().clone();
        // Backgrounds and decorations don't inherit in CSS, but they cover
        // the text inside: here they are the text's.
        let mut style = parent.clone();
        let block = is_block(name);
        let mut b = block.then(Block::default);
        let mut list = None;
        if is_hidden(name) {
            style.hidden = true;
        }
        self.element_style(name, token, &mut style, b.as_mut(), &mut list);
        // Style sheets, then the `style` attribute.
        let classes: Vec<&str> = token.attr("class").map(|c| c.split_whitespace().collect()).unwrap_or_default();
        let mut decls = if self.sheet.is_empty() { Vec::new() } else { self.sheet.matching(name, &classes) };
        if let Some(inline) = token.attr("style") {
            decls.extend(css::declarations(inline));
        }
        for (property, value) in &decls {
            self.apply(property, value, &parent, &mut style, &mut b, &mut list);
        }
        if let Some(b) = &mut b
            && let Some(em) = b.after_em
        {
            b.after = em * style.size;
        }
        let merged = block && !style.hidden && !ends_item_paragraph(name) && self.in_item();
        if block && !style.hidden && !merged {
            self.drop_block_break();
            self.flush_breaks();
            self.end_paragraph(false);
        }
        let is_pre = name == "pre" || name == "listing";
        self.stack.push(Frame { name: name.to_owned(), style, block: b, list, merged });
        if is_pre {
            self.after_pre = true;
        }
        if name == "li" && !self.hidden() {
            self.marker();
        }
    }

    /// What an element is before any CSS: the standard style sheet's part.
    fn element_style(
        &mut self,
        name: &str,
        token: &Token,
        s: &mut Style,
        b: Option<&mut Block>,
        list: &mut Option<List>,
    ) {
        let em = s.size;
        match name {
            "b" | "strong" | "th" => s.bold = true,
            "i" | "em" | "cite" | "var" | "dfn" | "address" => s.italic = true,
            "u" | "ins" => s.underline = true,
            "s" | "strike" | "del" => s.strike = true,
            "code" | "tt" | "kbd" | "samp" => s.families = vec!["monospace".into()],
            "pre" | "listing" | "xmp" | "plaintext" => {
                s.families = vec!["monospace".into()];
                s.pre = true;
            }
            "sup" | "sub" => {
                s.superscript = if name == "sup" { 1 } else { -1 };
                s.size = css::font_size("smaller", s.size).unwrap_or(s.size);
            }
            "small" => s.size = css::font_size("smaller", s.size).unwrap_or(s.size),
            "big" => s.size *= 1.2,
            "mark" => s.background = Some([1.0, 1.0, 0.0, 1.0]),
            "center" => s.align = Some(Align::Center),
            "span" => s.converted_space = token.attr("class") == Some("Apple-converted-space"),
            "a" => {
                if let Some(href) = token.attr("href") {
                    s.link = resolve(self.base.as_deref(), href);
                    s.color = Some([0.0, 0.0, 238.0 / 255.0, 1.0]);
                    s.underline = true;
                }
            }
            "font" => {
                if let Some(c) = token.attr("color").and_then(|c| css::color(c).ok().flatten()) {
                    s.color = Some(c);
                }
                if let Some(size) = token.attr("size").and_then(|v| font_tag_size(v, s.size)) {
                    s.size = size;
                    s.size_set = true;
                }
                if let Some(face) = token.attr("face") {
                    s.families = css::families(face);
                }
            }
            _ => {}
        }
        if let Some(level) =
            name.strip_prefix('h').and_then(|l| l.parse::<usize>().ok()).filter(|l| (1..=6).contains(l))
        {
            let (size, margin) = HEADINGS[level - 1];
            s.size = (size * em).round().max(9.0);
            s.bold = true;
            s.header = level as i64;
            if let Some(b) = b {
                b.after_em = Some(margin);
            }
            return;
        }
        s.header = 0;
        let Some(b) = b else { return };
        match name {
            "p" => b.after_em = Some(1.0),
            "blockquote" => {
                b.left = 40.0;
                b.right = 40.0;
                b.after_em = Some(1.0);
            }
            "ul" | "ol" | "menu" | "dir" => {
                let depth = self.stack.iter().filter(|f| f.list.is_some()).count();
                let kind = if name == "ol" {
                    token.attr("type").and_then(ListKind::from_type).unwrap_or(ListKind::Decimal)
                } else {
                    [ListKind::Disc, ListKind::Circle, ListKind::Square][depth.min(2)]
                };
                let start = token.attr("start").and_then(integer).unwrap_or(1);
                *list = Some(List { kind, next: i64::from(start) });
            }
            "li" => {
                let depth = self.stack.iter().filter(|f| f.list.is_some()).count().max(1);
                b.item = Some(depth);
            }
            "dd" => b.left = 40.0,
            _ => {}
        }
        if let Some(align) = token.attr("align") {
            s.align = text_align(align).or(s.align);
        }
        if let Some(dir) = token.attr("dir") {
            match dir.trim().to_ascii_lowercase().as_str() {
                "rtl" => s.direction = Direction::RightToLeft,
                "ltr" => s.direction = Direction::LeftToRight,
                _ => {}
            }
        }
    }

    /// Apply a CSS declaration.
    fn apply(
        &mut self,
        property: &str,
        value: &str,
        parent: &Style,
        s: &mut Style,
        b: &mut Option<Block>,
        list: &mut Option<List>,
    ) {
        let v = value.trim();
        if v.eq_ignore_ascii_case("inherit") {
            return;
        }
        match property {
            "color" => {
                if let Ok(c) = css::color(v) {
                    s.color = c;
                }
            }
            "background-color" | "background" => {
                let color = if property == "background" {
                    v.split_whitespace().find_map(|w| css::color(w).ok())
                } else {
                    css::color(v).ok()
                };
                if let Some(c) = color {
                    s.background = c.filter(|c| c[3] > 0.0);
                }
            }
            "font-family" => s.families = css::families(v),
            "font-size" => {
                if let Some(size) = css::font_size(v, parent.size) {
                    s.size = size;
                    s.size_set = true;
                }
            }
            "font-weight" => s.bold = css::bold(v, parent.bold),
            "font-style" => s.italic = matches!(v.to_ascii_lowercase().as_str(), "italic" | "oblique"),
            "font" => {
                if let Some(f) = css::font_shorthand(v) {
                    s.italic = f.style.is_some();
                    s.bold = f.weight.as_deref().is_some_and(|w| css::bold(w, parent.bold));
                    if let Some(size) = f.size.as_deref().and_then(|size| css::font_size(size, parent.size)) {
                        s.size = size;
                        s.size_set = true;
                    }
                    if let Some(families) = &f.families {
                        s.families = css::families(families);
                    }
                }
            }
            "text-decoration" | "text-decoration-line" => {
                let v = v.to_ascii_lowercase();
                if v.contains("none") {
                    s.underline = false;
                    s.strike = false;
                }
                s.underline |= v.contains("underline");
                s.strike |= v.contains("line-through");
            }
            "vertical-align" => match v.to_ascii_lowercase().as_str() {
                "super" => s.superscript = 1,
                "sub" => s.superscript = -1,
                "baseline" => {
                    s.superscript = 0;
                    s.baseline = 0.0;
                }
                other => {
                    if let Some(offset) = css::length(other, s.size, s.size) {
                        s.baseline = offset;
                    }
                }
            },
            "white-space" => s.pre = matches!(v.to_ascii_lowercase().as_str(), "pre" | "pre-wrap" | "break-spaces"),
            "display" => match v.to_ascii_lowercase().as_str() {
                "none" => s.hidden = true,
                "block" | "list-item" | "flex" | "grid" | "table" if b.is_none() => *b = Some(Block::default()),
                _ => {}
            },
            "text-align" => s.align = text_align(v).or(s.align),
            "direction" => match v.to_ascii_lowercase().as_str() {
                "rtl" => s.direction = Direction::RightToLeft,
                "ltr" => s.direction = Direction::LeftToRight,
                _ => {}
            },
            "list-style-type" | "list-style" => {
                if let Some(l) = list
                    && let Some(kind) = v.split_whitespace().find_map(ListKind::from_css)
                {
                    l.kind = kind;
                }
            }
            _ => {}
        }
        let Some(b) = b else { return };
        let len = |v: &str| css::length(v, s.size, 0.0);
        match property {
            "margin" => {
                let [_, right, bottom, left] = css::sides(v);
                b.right = len(right).unwrap_or(b.right);
                if let Some(after) = len(bottom) {
                    (b.after, b.after_em) = (after, None);
                }
                b.left = len(left).unwrap_or(b.left);
            }
            "margin-bottom" => {
                if let Some(after) = len(v) {
                    (b.after, b.after_em) = (after, None);
                }
            }
            "margin-left" => b.left = len(v).unwrap_or(b.left),
            "margin-right" => b.right = len(v).unwrap_or(b.right),
            "padding-left" => b.left += len(v).unwrap_or(0.0),
            "text-indent" => b.indent = len(v).unwrap_or(b.indent),
            _ => {}
        }
    }

    fn end(&mut self, name: &str) {
        let Some(at) = self.stack.iter().rposition(|f| f.name == name) else { return };
        if at == 0 {
            return;
        }
        while self.stack.len() > at {
            let hidden = self.hidden();
            let frame = self.stack.last().expect("a frame above the root");
            let (block, merged) = (frame.block.is_some(), frame.merged);
            let item_start = frame.block.as_ref().and_then(|b| b.marker.as_ref()).map(|(_, at)| *at);
            if block && !hidden && merged {
                // A block in a list item: a break that ends it shows
                // nothing, and it ends a line (U+2028) if more text follows
                // in the item.
                self.breaks.pop();
                self.flush_breaks();
                if !self.fresh {
                    self.breaks.push(Break::Block);
                    self.block_style = Some(self.style().char_style());
                }
            } else if block && !hidden {
                // A break that ends its block shows nothing, but a block
                // holding only breaks is a line.
                if self.breaks.pop().is_some() {
                    self.flush_breaks();
                    if self.out.at_paragraph_start() {
                        self.end_paragraph(true);
                    }
                }
                if item_start == Some(self.out.len()) {
                    // An item with nothing in it is its marker alone.
                    self.open_line();
                }
                self.end_paragraph(false);
            }
            self.stack.pop();
        }
        self.after_pre = false;
        // A list item's text after a nested list, or after a table's cell
        // or a preformatted line, gets the item's marker again.
        self.marker = if self.out.at_paragraph_start() { self.item_marker() } else { None };
    }

    /// Whether what is read now is in a list item (rather than a list, or
    /// neither).
    fn in_item(&self) -> bool {
        self.item_frame().is_some()
    }

    /// The list item what is read now is in (not in a list nested in it).
    fn item_frame(&self) -> Option<&Frame> {
        self.stack
            .iter()
            .rev()
            .find(|f| f.list.is_some() || f.block.as_ref().is_some_and(|b| b.item.is_some()))
            .filter(|f| f.list.is_none())
    }

    fn item_marker(&self) -> Option<String> {
        self.item_frame().and_then(|f| f.block.as_ref()?.marker.as_ref()).map(|(m, _)| m.clone())
    }

    /// A paragraph ends: a list item's block's end that was waiting for
    /// text shows nothing.
    fn drop_block_break(&mut self) {
        if self.breaks.last() == Some(&Break::Block) {
            self.breaks.pop();
        }
    }

    /// Put in the line breaks waiting.
    fn flush_breaks(&mut self) {
        for b in std::mem::take(&mut self.breaks) {
            match b {
                Break::Paragraph => self.end_paragraph(true),
                Break::Line | Break::Block => {
                    self.open_line();
                    let block_style = if b == Break::Block { self.block_style.take() } else { None };
                    let style = block_style.unwrap_or_else(|| self.style().char_style());
                    let style = CharStyle { link: None, underline: 0, strikethrough: 0, ..style };
                    self.out.push("\u{2028}", &style);
                    self.fresh = true;
                }
            }
        }
    }

    /// A list item's marker, between tabs: it goes in with the item's
    /// first text. Items are numbered in turn from the list's start (an
    /// item's `value` is ignored, as macOS ignores it).
    fn marker(&mut self) {
        let Some(list_at) = self.stack.iter().rposition(|f| f.list.is_some()) else { return };
        let list = self.stack[list_at].list.as_mut().expect("a list");
        let marker = format!("\t{}\t", list.kind.marker(list.next));
        list.next = list.next.saturating_add(1);
        let at = self.out.len();
        if let Some(b) = self.stack.last_mut().and_then(|f| f.block.as_mut()) {
            b.marker = Some((marker.clone(), at));
        }
        self.marker = Some(marker);
        self.item_style = None;
        self.fresh = true;
    }

    /// Before a line's first text: a list item's marker, in the text's
    /// style (without its link, underline or strikethrough), and the item
    /// paragraph's style from the blocks around the text.
    fn open_line(&mut self) {
        let Some(marker) = self.marker.take() else { return };
        let style = self.style().char_style();
        let style = CharStyle { underline: 0, strikethrough: 0, link: None, ..style };
        self.out.push(&marker, &style);
        if self.item_style.is_none() {
            self.item_style = Some(self.paragraph_style());
        }
    }

    fn text(&mut self, text: &str) {
        if self.hidden() {
            return;
        }
        let style = self.style().char_style();
        if self.style().pre {
            let mut text = text.replace("\r\n", "\n").replace('\r', "\n");
            if std::mem::take(&mut self.after_pre) && text.starts_with('\n') {
                text.remove(0);
            }
            self.flush_breaks();
            let mut first = true;
            for line in text.split('\n') {
                if !first {
                    self.end_paragraph(true);
                }
                first = false;
                if line.is_empty() {
                    continue;
                }
                self.open_line();
                if let Some(space) = self.space.take() {
                    self.out.push(" ", &space);
                }
                self.out.push(line, &style);
                self.fresh = false;
            }
            return;
        }
        self.after_pre = false;
        let converted = self.style().converted_space;
        let mut word = String::new();
        for c in text.chars() {
            if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0C}') {
                if !word.is_empty() {
                    self.push_word(&std::mem::take(&mut word), &style);
                }
                if self.space.is_none() && !self.fresh {
                    self.space = Some(style.clone());
                }
            } else if converted && c == '\u{A0}' {
                // An `Apple-converted-space`'s no-break space: a space that
                // stays.
                word.push(' ');
            } else {
                word.push(c);
            }
        }
        if !word.is_empty() {
            self.push_word(&word, &style);
        }
    }

    fn push_word(&mut self, word: &str, style: &CharStyle) {
        self.flush_breaks();
        self.open_line();
        if let Some(space) = self.space.take() {
            self.out.push(" ", &space);
        }
        self.out.push(word, style);
        self.fresh = false;
    }

    /// End the paragraph being read: always (`<br>`), or only if it has
    /// text (a block's end).
    fn end_paragraph(&mut self, always: bool) {
        self.space = None;
        if !always && self.out.at_paragraph_start() {
            return;
        }
        if always {
            self.open_line();
        }
        self.fresh = true;
        let style = self.style().char_style();
        let style = CharStyle { link: None, underline: 0, strikethrough: 0, ..style };
        let para = self.item_style.take().unwrap_or_else(|| self.paragraph_style());
        self.out.end_paragraph("\n", &style, Some(&para));
        self.marker = self.item_marker();
    }

    /// The style of the paragraph being read, from the blocks it is in.
    fn paragraph_style(&self) -> ParaStyle {
        let s = self.style();
        let mut left = 0.0;
        let mut right = 0.0;
        let mut item = None;
        for f in &self.stack {
            if let Some(b) = &f.block {
                left += b.left;
                right += b.right;
                if b.item.is_some() {
                    item = b.item;
                }
            }
        }
        let innermost = self.stack.iter().rev().find_map(|f| f.block.as_ref()).cloned().unwrap_or_default();
        let mut p = ParaStyle {
            alignment: s.align.unwrap_or(Align::Natural),
            direction: s.direction,
            tabs: Vec::new(),
            default_tab_interval: 36.0,
            header_level: s.header,
            // A list item's own margin isn't its paragraphs' spacing; a
            // block's in it is.
            paragraph_spacing: if innermost.item.is_some() { 0.0 } else { innermost.after },
            head_indent: left,
            first_line_head_indent: left + innermost.indent,
            tail_indent: -right,
            ..ParaStyle::default()
        };
        if let Some(depth) = item {
            // Indented by the item's depth alone (not by the list's margins
            // or its blocks'), with a block's in the item right margin,
            // left-aligned unless a block says; the marker's tab stop 11
            // points into the item's level, and the text's at the next
            // level (where the first line starts doesn't show, after a
            // tab), as macOS has them.
            let depth = depth as f64;
            p.alignment = s.align.unwrap_or(Align::Left);
            p.head_indent = 36.0 * depth;
            p.first_line_head_indent = innermost.indent;
            p.tail_indent = if innermost.item.is_some() { 0.0 } else { -innermost.right };
            p.tabs = vec![
                Tab { location: 36.0 * (depth - 1.0) + 11.0, kind: TabKind::Left },
                Tab { location: p.head_indent, kind: TabKind::Left },
            ];
        }
        p
    }

    fn finish(mut self) -> Doc {
        // Blocks left open end here.
        while self.stack.len() > 1 {
            let name = self.stack.last().map(|f| f.name.clone()).unwrap_or_default();
            self.end(&name);
        }
        self.space = None;
        // Breaks outside any block show, the last one too.
        self.drop_block_break();
        self.flush_breaks();
        let para = self.paragraph_style();
        let mut doc = self.out.finish(Some(&para));
        doc.attrs.cocoa_version = self.cocoa_version;
        doc
    }
}

fn text_align(v: &str) -> Option<Align> {
    Some(match v.trim().to_ascii_lowercase().as_str() {
        "left" => Align::Left,
        "right" => Align::Right,
        "center" | "middle" => Align::Center,
        "justify" => Align::Justified,
        "start" => Align::Natural,
        _ => return None,
    })
}

/// An integer attribute, as HTML reads one: after white space, a sign and
/// digits, whatever follows them; none without digits, or past 32 bits.
fn integer(v: &str) -> Option<i32> {
    let v = v.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (negative, digits) = match v.as_bytes().first() {
        Some(b'-') => (true, &v[1..]),
        Some(b'+') => (false, &v[1..]),
        _ => (false, v),
    };
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    let n: i64 = digits[..end].parse().ok()?;
    i32::try_from(if negative { -n } else { n }).ok()
}

/// A `<font size>`: 1 to 7, or relative to 3 with a sign (in 32 bits, as
/// macOS adds it).
fn font_tag_size(v: &str, _parent: f64) -> Option<f64> {
    let n = integer(v)?;
    let n = if v.trim_start().starts_with(['+', '-']) { n.wrapping_add(3) } else { n };
    Some(match n.clamp(1, 7) {
        1 => 9.0,
        2 => 10.0,
        3 => 12.0,
        4 => 14.0,
        5 => 18.0,
        6 => 24.0,
        _ => 37.0,
    })
}

/// A link's target made absolute against `base`; None for a relative
/// target without one (which AppKit drops), or for an empty one.
pub(crate) fn resolve(base: Option<&str>, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    if scheme(href).is_some() {
        return Some(href.to_owned());
    }
    let base = base?;
    let scheme_end = scheme(base)?.len() + 1;
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("{}//{rest}", &base[..scheme_end]));
    }
    // The base's authority, and its path without query or fragment.
    let after_scheme = &base[scheme_end..];
    let (authority, path) = match after_scheme.strip_prefix("//") {
        Some(a) => {
            let end = a.find(['/', '?', '#']).unwrap_or(a.len());
            (format!("//{}", &a[..end]), &a[end..])
        }
        None => (String::new(), after_scheme),
    };
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    let prefix = format!("{}{authority}", &base[..scheme_end]);
    if href.starts_with('#') || href.starts_with('?') {
        let base_no_fragment = &base[..base.find('#').unwrap_or(base.len())];
        let base_no_query = if href.starts_with('?') {
            &base_no_fragment[..base_no_fragment.find('?').unwrap_or(base_no_fragment.len())]
        } else {
            base_no_fragment
        };
        return Some(format!("{base_no_query}{href}"));
    }
    let joined = if href.starts_with('/') {
        href.to_owned()
    } else {
        let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
        let dir = if dir.is_empty() { "/" } else { dir };
        format!("{dir}{href}")
    };
    // Remove dot segments.
    let (path_part, tail) = joined.split_at(joined.find(['?', '#']).unwrap_or(joined.len()));
    let mut segments: Vec<&str> = Vec::new();
    let parts: Vec<&str> = path_part.split('/').collect();
    for (i, seg) in parts.iter().enumerate() {
        match *seg {
            "." => {
                if i == parts.len() - 1 {
                    segments.push("");
                }
            }
            ".." => {
                if segments.len() > 1 {
                    segments.pop();
                }
                if i == parts.len() - 1 {
                    segments.push("");
                }
            }
            s => segments.push(s),
        }
    }
    Some(format!("{prefix}{}{tail}", segments.join("/")))
}

/// A URL's scheme, if it starts with one.
fn scheme(url: &str) -> Option<&str> {
    let colon = url.find(':')?;
    let s = &url[..colon];
    (s.len() > 1
        && s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)))
    .then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(html: &str) -> Doc {
        read(html, &Options::default())
    }

    fn runs(d: &Doc) -> Vec<(&str, &CharStyle)> {
        d.run_ranges().map(|(r, s)| (&d.text[r], s)).collect()
    }

    fn font(s: &CharStyle) -> &Font {
        s.font.as_ref().unwrap()
    }

    #[test]
    fn inline_formatting() {
        let d = doc(
            "<p>Hi <b>there</b> <i>it</i> <u>u</u> <s>s</s> <strike>st</strike> <del>d</del> <em>em</em> <strong>strong</strong></p><p>second</p>",
        );
        assert_eq!(d.text, "Hi there it u s st d em strong\nsecond\n");
        let r = runs(&d);
        let get = |t: &str| r.iter().find(|(x, _)| *x == t).unwrap().1;
        assert!(font(get("there")).bold && !font(get("Hi ")).bold);
        assert!(font(get("it")).italic && font(get("em")).italic && font(get("strong")).bold);
        assert_eq!(
            (get("u").underline, get("s").strikethrough, get("st").strikethrough, get("d").strikethrough),
            (1, 1, 1, 1)
        );
        let f = font(get("Hi "));
        assert_eq!((f.names.as_slice(), f.generic, f.size), (&["Times".to_string()][..], Generic::Serif, 12.0));
        let p = d.paras[0].1.as_ref().unwrap();
        assert_eq!(
            (p.paragraph_spacing, p.default_tab_interval, p.direction, p.tabs.len()),
            (12.0, 36.0, Direction::LeftToRight, 0)
        );
        assert_eq!(d.paras.len(), 2);
    }

    #[test]
    fn headings_code_and_links() {
        let d = doc(
            "<h1>H1</h1><h3>H3</h3><h6>H6</h6>text <code>code</code> <a href=\"https://example.com/x\">link <b>b</b></a> <a href=\"rel\">rel</a>",
        );
        assert_eq!(d.text, "H1\nH3\nH6\ntext code link b rel");
        let r = runs(&d);
        let get = |t: &str| r.iter().find(|(x, _)| *x == t).unwrap().1;
        assert_eq!((font(get("H1\n")).size, font(get("H1\n")).bold), (24.0, true));
        assert_eq!((font(get("H3\n")).size, font(get("H6\n")).size), (14.0, 9.0));
        assert_eq!(d.paras[0].1.as_ref().unwrap().header_level, 1);
        assert!((d.paras[0].1.as_ref().unwrap().paragraph_spacing - 16.08).abs() < 0.01);
        let code = font(get("code"));
        assert_eq!((code.generic, code.size), (Generic::Mono, 13.0));
        assert_eq!(get("link ").link.as_deref(), Some("https://example.com/x"));
        assert_eq!(get("b").link.as_deref(), Some("https://example.com/x"));
        assert!(font(get("b")).bold && get("b").underline == 1);
        assert_eq!(get("link ").color.as_ref().map(|c| c.srgb), Some([0.0, 0.0, 238.0 / 255.0, 1.0]));
        assert_eq!(get("rel").link, None, "relative, without a base");
        let based = read(
            "<a href=\"../y?q#f\">l</a>",
            &Options { base_url: Some("https://h.example/a/b/c".into()), ..Options::default() },
        );
        assert_eq!(runs(&based)[0].1.link.as_deref(), Some("https://h.example/a/y?q#f"));
    }

    #[test]
    fn white_space_and_breaks() {
        assert_eq!(
            doc("<p>a   b</p>\n\n<p> lead and trail </p><p>x<b> bold </b>y</p>").text,
            "a b\nlead and trail\nx bold y\n"
        );
        assert_eq!(doc("line<br>break<br/>again").text, "line\nbreak\nagain");
        assert_eq!(doc("<p>a<br></p><p><br></p><p>b</p>").text, "a\n\nb\n");
        assert_eq!(
            doc("<p>a<br> b<br><br>c</p><div>d<br>e</div><p>a</p><br><p>b</p>").text,
            "a\u{2028}b\u{2028}\u{2028}c\nd\ne\na\n\nb\n"
        );
        assert_eq!(doc("<ul><li>a<br>b</li></ul>x<br>").text, "\t•\ta\u{2028}b\nx\n");
        assert_eq!(
            doc("<div>div one</div><div>div two</div>plain   spaced\n  text").text,
            "div one\ndiv two\nplain spaced text"
        );
        assert_eq!(doc("<pre>\ncode  block\n  indented</pre><p>x</p>").text, "code  block\n  indented\nx\n");
        let d = doc("x<b> bold </b>y");
        assert!(font(runs(&d)[1].1).bold && runs(&d)[1].0 == " bold ");
    }

    #[test]
    fn lists_and_tables() {
        let d = doc("<ul><li>one<ul><li>nested</li></ul></li><li>two</li></ul><ol start=3><li>three<li>four</ol>after");
        assert_eq!(d.text, "\t•\tone\n\t◦\tnested\n\t•\ttwo\n\t3\tthree\n\t4\tfour\nafter");
        let p = d.paras[1].1.as_ref().unwrap();
        assert_eq!((p.head_indent, p.first_line_head_indent), (72.0, 0.0));
        assert_eq!(p.tabs.iter().map(|t| t.location).collect::<Vec<_>>(), [47.0, 72.0]);
        assert_eq!(doc("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>x").text, "a\nb\nc\nx");
    }

    /// Blocks in list items, as macOS reads them (Google Docs, GitHub and
    /// Confluence put `<p>`s in `<li>`s).
    #[test]
    fn list_items_hold_blocks() {
        let para = |d: &Doc, i: usize| d.paras[i].1.clone().unwrap();
        let d = doc("<ul><li><p>para item</p></li></ul>");
        assert_eq!(d.text, "\t•\tpara item\n");
        assert_eq!((para(&d, 0).paragraph_spacing, para(&d, 0).head_indent), (12.0, 36.0));
        assert_eq!(doc("<ul><li><div>div item</div></li></ul>").text, "\t•\tdiv item\n");
        let d = doc("<ul>\n<li>\n<p>one</p>\n</li>\n<li>\n<p>two</p>\n</li>\n</ul>");
        assert_eq!(d.text, "\t•\tone\n\t•\ttwo\n");
        assert_eq!(d.paras.len(), 2);
        // Blocks in an item are lines of its paragraph, U+2028 where one
        // ends; the style is the first text's block's.
        let d = doc("<ol><li><p style=\"margin-bottom:30px\">x</p><p>y</p></li><li>text<p>then p</p></li></ol>");
        assert_eq!(d.text, "\t1\tx\u{2028}y\n\t2\ttextthen p\n");
        assert_eq!((para(&d, 0).paragraph_spacing, para(&d, 1).paragraph_spacing), (30.0, 0.0));
        let d = doc("<ul><li><h2>h</h2><p>x</p></li><li><p style=\"text-align:center\">c</p></li></ul>");
        assert_eq!(d.text, "\t•\th\u{2028}x\n\t•\tc\n");
        assert_eq!((para(&d, 0).header_level, para(&d, 1).alignment), (2, Align::Center));
        // (The line break in the heading's style, the item left-aligned.)
        assert!(font(runs(&d)[0].1).bold && runs(&d)[0].0 == "\t•\th\u{2028}");
        assert_eq!(para(&doc("<ul><li>x</li></ul>"), 0).alignment, Align::Left);
        // Items are indented by their depth, whatever the margins around
        // them; a block in one gives its right margin.
        let d = doc("<ul><li><blockquote>q</blockquote></li></ul><blockquote><ul><li>x</li></ul></blockquote>");
        let (q, x) = (para(&d, 0), para(&d, 1));
        assert_eq!(
            (q.head_indent, q.first_line_head_indent, q.tail_indent, q.paragraph_spacing),
            (36.0, 0.0, -40.0, 12.0)
        );
        assert_eq!(q.tabs.iter().map(|t| t.location).collect::<Vec<_>>(), [11.0, 36.0]);
        assert_eq!((x.head_indent, x.tail_indent), (36.0, 0.0));
        // The marker takes the first text's style, but not its link or
        // underline.
        let d = doc("<ul><li><b><u>bold</u></b> rest</li></ul>");
        let r = runs(&d);
        assert_eq!((r[0].0, font(r[0].1).bold, r[0].1.underline), ("\t•\t", true, 0));
        // Nested lists, tables and preformatted lines end the item's
        // paragraph, and what follows them gets the marker again; an empty
        // item is its marker, one holding only a list has none.
        assert_eq!(
            doc("<ul><li><p>one</p><ul><li>n</li></ul>after</li><li></li><li><ul><li>x</li></ul></li></ul>").text,
            "\t•\tone\n\t◦\tn\n\t•\tafter\n\t•\t\n\t◦\tx\n"
        );
        assert_eq!(doc("<ul><li><pre>a\nb</pre></li></ul>").text, "\t•\ta\n\t•\tb\n");
        assert_eq!(doc("<ul><li><table><tr><td>a</td><td>b</td></tr></table></li></ul>").text, "\t•\ta\n\t•\tb\n");
        assert_eq!(
            doc("<ul><li></li><li><br></li><li><p></p></li><li>b</li></ul>").text,
            "\t•\t\n\t•\t\n\t•\t\n\t•\tb\n"
        );
    }

    #[test]
    fn list_markers_and_numbers() {
        let text = |html: &str| doc(html).text;
        assert_eq!(text("<ol type=\"A\"><li>a<li>b</ol>"), "\tA\ta\n\tB\tb\n");
        assert_eq!(text("<ol type=\"I\"><li>a<li>b</ol>"), "\tI\ta\n\tII\tb\n");
        assert_eq!(text("<ol type=\"i\" start=\"3999\"><li>a<li>b</ol>"), "\tmmmcmxcix\ta\n\t\tb\n");
        assert_eq!(text("<ol type=\"i\" start=\"-3\"><li>a</ol>"), "\t-3\ta\n");
        assert_eq!(text("<ol type=\"i\" start=\"2000000\"><li>a</ol>"), "\t\ta\n");
        assert_eq!(text("<ol style=\"list-style-type: UPPER-ALPHA\"><li>a</ol>"), "\tA\ta\n");
        // Letters go round, and back through ASCII below `a`.
        let letters: Vec<String> =
            [0, 27, -5, -100, 2147483647].iter().map(|n| text(&format!("<ol type=a start={n}><li>x</ol>"))).collect();
        assert_eq!(letters, ["\t`\tx\n", "\ta\tx\n", "\t[\tx\n", "\tJ\tx\n", "\tw\tx\n"]);
        assert_eq!(text("<ol type=A start=-30><li>x</ol>"), "\t<\tx\n");
        // Numbers as HTML reads integers; `value` ignored.
        assert_eq!(text("<ol start=\" +5abc\"><li>a<li>b</ol>"), "\t5\ta\n\t6\tb\n");
        assert_eq!(text("<ol start=\"2147483647\"><li>a<li>b</ol>"), "\t2147483647\ta\n\t2147483648\tb\n");
        for start in ["2147483648", "9223372036854775807", "99999999999999999999999", "x"] {
            assert_eq!(text(&format!("<ol start=\"{start}\"><li>a</ol>")), "\t1\ta\n", "{start}");
        }
        assert_eq!(
            text("<ol><li value=\"0\">a<li value=\"-7\">b<li value=\"9\">c<li>d</ol>"),
            "\t1\ta\n\t2\tb\n\t3\tc\n\t4\td\n"
        );
    }

    /// Numbers at the ends of their ranges and past them never overflow.
    #[test]
    fn extreme_numbers() {
        let size = |v: &str| font(runs(&doc(&format!("<font size=\"{v}\">x</font>")))[0].1).size;
        let sizes: Vec<f64> =
            ["1", "2", "3", "4", "5", "6", "7", "0", "99", "-1", "+1", "+4"].iter().map(|v| size(v)).collect();
        assert_eq!(sizes, [9.0, 10.0, 12.0, 14.0, 18.0, 24.0, 37.0, 9.0, 37.0, 10.0, 14.0, 37.0]);
        // As macOS adds, in 32 bits.
        assert_eq!(size("+2147483647"), 9.0);
        for v in ["+9223372036854775807", "-9223372036854775808", "+99999999999999999999"] {
            assert_eq!(size(v), 12.0, "{v}");
        }
        let d = doc(&format!("<ol type=i start=2147483647>{}</ol>", "<li>x".repeat(3)));
        assert_eq!(d.text, "\t\tx\n".repeat(3));
    }

    #[test]
    fn breaks_and_spaces_outside_blocks() {
        assert_eq!(doc("<html><body>\n<!--StartFragment--><b>x</b> y<!--EndFragment-->\n</body></html>").text, "x y");
        assert_eq!(doc("<html><body>a<div>b</div>c</body></html>").text, "a\nb\nc");
        assert_eq!(doc("a<br>b<br>").text, "a\nb\n");
        assert_eq!(doc("<p>a</p><br>").text, "a\n\n");
        assert_eq!(doc("<span>a<br></span>").text, "a\n");
        assert_eq!(doc("<br><br>").text, "\n\n");
        assert_eq!(doc("<div>a<br></div>").text, "a\n");
        // An `Apple-converted-space` span's no-break spaces are spaces; other
        // no-break spaces stay.
        assert_eq!(
            doc("<span class=\"Apple-converted-space\">&nbsp;</span>a<span class=\"Apple-converted-space\">&nbsp; </span>b&nbsp;c").text,
            " a  b\u{A0}c"
        );
        assert_eq!(doc("<span class=\"x Apple-converted-space\">&nbsp;</span>a").text, "\u{A0}a");
    }

    #[test]
    fn styles_and_sheets() {
        let d = doc(
            "<html><head><style>p.p1 {margin: 0.0px 20.0px 3.0px 10.0px; text-indent: 5.0px; text-align: center; font: 13.0px Helvetica} span.s1 {color: #fb0007; background-color: rgba(0, 0, 255, 0.5)}</style></head><body><p class=\"p1\">a<span class=\"s1\">b</span><span style=\"font-size: 11pt; font-weight: 700; font-family: 'Courier New', monospace; text-decoration: underline line-through; vertical-align: super\">c</span></p></body></html>",
        );
        assert_eq!(d.text, "abc\n");
        let r = runs(&d);
        let a = font(r[0].1);
        assert_eq!((a.names.as_slice(), a.size), (&["Helvetica".to_string()][..], 13.0));
        let b = r[1].1;
        assert!((b.color.as_ref().unwrap().srgb[0] - 251.0 / 255.0).abs() < 1e-9);
        assert_eq!(b.background.as_ref().unwrap().srgb, [0.0, 0.0, 1.0, 0.5]);
        let c = r[2].1;
        assert_eq!(
            (font(c).names.as_slice(), font(c).bold, font(c).generic),
            (&["Courier New".to_string()][..], true, Generic::Mono)
        );
        assert!((font(c).size - 44.0 / 3.0).abs() < 1e-9);
        assert_eq!((c.underline, c.strikethrough, c.superscript), (1, 1, 1));
        let p = d.paras[0].1.as_ref().unwrap();
        assert_eq!(
            (p.alignment, p.head_indent, p.first_line_head_indent, p.tail_indent, p.paragraph_spacing),
            (Align::Center, 10.0, 15.0, -20.0, 3.0)
        );
        // Black is no color; `<font>` and `align` work.
        let d = doc(
            "<span style=\"color:black\">k</span><font color=red size=5 face=Courier>f</font><p align=right dir=rtl>r</p>",
        );
        let r = runs(&d);
        assert!(r[0].1.color.is_none());
        assert_eq!((r[1].1.color.as_ref().unwrap().srgb, font(r[1].1).size), ([1.0, 0.0, 0.0, 1.0], 18.0));
        let p = d.paras.last().unwrap().1.as_ref().unwrap();
        assert_eq!((p.alignment, p.direction), (Align::Right, Direction::RightToLeft));
        // Hidden content.
        assert_eq!(doc("a<script>b</script><span style=\"display:none\">c</span><template>d</template>e").text, "ae");
    }

    #[test]
    fn encodings() {
        let o = Options::default();
        assert_eq!(decode_bytes(b"caf\xc3\xa9", &o), "cafÃ©", "Windows-1252 unless told");
        assert_eq!(decode_bytes(b"<meta charset=\"utf-8\">caf\xc3\xa9", &o), "<meta charset=\"utf-8\">café");
        assert_eq!(
            decode_bytes(b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\">\xc3\xa9", &o)
                .chars()
                .last(),
            Some('é')
        );
        assert_eq!(decode_bytes(b"\xef\xbb\xbfcaf\xc3\xa9", &o), "café");
        assert_eq!(decode_bytes(b"\xff\xfeh\0i\0", &o), "hi");
        assert_eq!(decode_bytes(b"caf\xc3\xa9", &Options { encoding: Some(Encoding::Utf8), ..o.clone() }), "café");
        assert!(is_html(b"  <!DOCTYPE HTML><p>") && is_html(b"<html>") && !is_html(b"<b>x</b>"));
        assert_eq!(doc("<meta name=\"CocoaVersion\" content=\"2685.7\"><p>x</p>").attrs.cocoa_version, Some(2685.7));
    }
}
