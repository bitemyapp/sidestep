//! Writing a [`Doc`] as RTF, laid out as AppKit writes it on macOS (as
//! measured there: exact bytes don't matter to readers, but the structure
//! and Cocoa's own control words do, to macOS programs that read what a
//! Sidestep program copies):
//!
//! - the header names Windows-1252 and Cocoa's RTF version, then the font
//!   table (PostScript names, a family class each), the color table (auto,
//!   white, then the colors used, as 8-bit RGB) and Cocoa's extended color
//!   table beside it with each color in its own space (`\cssrgb`,
//!   `\csgenericrgb`, `\cspthree`, `\csgray`, `\cscmyk`; a system color
//!   with its name, `\cname`);
//! - the document's information and page (`{\info…}`, `\paperw`, margins,
//!   `\viewkind`, `\deftab`, …) from the document attributes;
//! - a `\pard` with the paragraph's formatting where a paragraph's style
//!   differs from the one before (tab stops first, the twelve default ones
//!   included; a positive tail indent as the right indent it leaves on the
//!   page), each paragraph ending in `\` and a line end;
//! - character formatting as it changes from run to run, a font change on a
//!   line of its own; bold and italic from the font's traits; links as
//!   `HYPERLINK` fields around their text;
//! - text in Windows-1252 where it can be (`\'hh` above ASCII), else `\uN`
//!   after one `\uc0`, surrogate pairs as two; U+2028 as `\u8232`.

use std::fmt::Write;

use super::model::{
    Align, CharStyle, Color, Direction, Doc, DocAttrs, Font, Generic, PAPER, ParaStyle, Space, TabKind,
};
use super::tables::CodePage;

/// The Cocoa RTF version written: macOS 26's.
pub(crate) const COCOA_VERSION: u32 = 2870;

/// `doc` as RTF.
pub(crate) fn write(doc: &Doc) -> Vec<u8> {
    let mut w = Writer::new(doc);
    w.header(&doc.attrs);
    if !doc.text.is_empty() {
        w.body(doc);
    }
    w.out.push('}');
    w.out.into_bytes()
}

/// A run's font: its own, or Helvetica 12, which AppKit writes for text
/// without one.
fn font_of(style: &CharStyle) -> Font {
    style
        .font
        .clone()
        .unwrap_or_else(|| Font { family: "Helvetica".into(), ..Font::named("Helvetica", Generic::Sans, 12.0) })
}

/// A font table entry: a font's name and family class.
#[derive(Clone, Debug, PartialEq)]
struct FontEntry {
    name: String,
    generic: Generic,
}

struct Writer {
    out: String,
    fonts: Vec<FontEntry>,
    /// Colors after auto and white: index `i` here is `\cf(i + 2)`.
    colors: Vec<Color>,
    /// Whether `\uc0` is in effect.
    uc0: bool,
}

/// The formatting written so far, to write only what changes.
#[derive(Clone, Debug, PartialEq)]
struct Written {
    font: Option<usize>,
    size: f64,
    bold: bool,
    italic: bool,
    color: usize,
    background: usize,
    underline: i64,
    underline_color: usize,
    strikethrough: i64,
    strikethrough_color: usize,
    superscript: i64,
    baseline: i64,
    kern: Option<(i64, i64)>,
    shadow: Option<(i64, i64, i64, i64, usize)>,
    expansion: i64,
    obliqueness: i64,
    stroke: Option<(i64, usize)>,
    ligature: Option<i64>,
}

impl Written {
    fn start() -> Written {
        Written {
            font: None,
            size: 12.0,
            bold: false,
            italic: false,
            color: usize::MAX,
            background: 1,
            underline: 0,
            underline_color: 0,
            strikethrough: 0,
            strikethrough_color: 0,
            superscript: 0,
            baseline: 0,
            kern: None,
            shadow: None,
            expansion: 0,
            obliqueness: 0,
            stroke: None,
            ligature: None,
        }
    }
}

fn twips(points: f64) -> i64 {
    (points * 20.0).round() as i64
}

/// The family class RTF names for a font.
fn class(generic: Generic) -> &'static str {
    match generic {
        Generic::Serif => "froman",
        Generic::Mono => "fmodern",
        Generic::Sans => "fswiss",
        Generic::System => "fnil",
    }
}

impl Writer {
    fn new(doc: &Doc) -> Writer {
        let mut w = Writer { out: String::new(), fonts: Vec::new(), colors: Vec::new(), uc0: false };
        for (_, style) in doc.run_ranges() {
            w.font_index(&font_of(style));
            for c in [
                &style.color,
                &style.background,
                &style.underline_color,
                &style.strikethrough_color,
                &style.stroke_color,
            ]
            .into_iter()
            .flatten()
            {
                w.color_index(c);
            }
            if let Some(Some(c)) = style.shadow.as_ref().map(|s| &s.color) {
                w.color_index(&opaque(c));
            }
        }
        w
    }

    fn font_index(&mut self, font: &Font) -> usize {
        let entry = FontEntry {
            name: font.names.first().cloned().unwrap_or_else(|| "Helvetica".into()),
            generic: font.generic,
        };
        match self.fonts.iter().position(|f| *f == entry) {
            Some(i) => i,
            None => {
                self.fonts.push(entry);
                self.fonts.len() - 1
            }
        }
    }

    /// A color's index in the color table (2 on: 0 is auto, 1 white).
    fn color_index(&mut self, color: &Color) -> usize {
        match self.colors.iter().position(|c| c == color) {
            Some(i) => i + 2,
            None => {
                self.colors.push(color.clone());
                self.colors.len() + 1
            }
        }
    }

    fn header(&mut self, attrs: &DocAttrs) {
        let _ = writeln!(self.out, "{{\\rtf1\\ansi\\ansicpg1252\\cocoartf{COCOA_VERSION}");
        if attrs.read_only == Some(1) {
            self.out.push_str("\\readonlydoc1");
        }
        self.out.push_str("\\cocoatextscaling0\\cocoaplatform0{\\fonttbl");
        let fonts = std::mem::take(&mut self.fonts);
        let mut line = self.out.len();
        for (i, f) in fonts.iter().enumerate() {
            if self.out.len() - line > 64 {
                self.out.push('\n');
                line = self.out.len();
            }
            let _ = write!(self.out, "\\f{i}\\{}\\fcharset0 ", class(f.generic));
            self.text(&f.name);
            self.out.push(';');
        }
        self.fonts = fonts;
        // The table's group ends, and a `\uc0` in it with it.
        self.uc0 = false;
        self.out.push_str("}\n{\\colortbl;\\red255\\green255\\blue255;");
        let colors = std::mem::take(&mut self.colors);
        line = self.out.len();
        for c in &colors {
            if self.out.len() - line > 64 {
                self.out.push('\n');
                line = self.out.len();
            }
            let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            let _ = write!(self.out, "\\red{}\\green{}\\blue{};", byte(c.srgb[0]), byte(c.srgb[1]), byte(c.srgb[2]));
        }
        self.out.push_str("}\n{\\*\\expandedcolortbl;;");
        line = self.out.len();
        for c in &colors {
            if self.out.len() - line > 64 {
                self.out.push('\n');
                line = self.out.len();
            }
            self.out.push_str(match c.space {
                Space::Srgb => "\\cssrgb",
                Space::GenericRgb => "\\csgenericrgb",
                Space::DisplayP3 => "\\cspthree",
                Space::Gray => "\\csgray",
                Space::Cmyk => "\\cscmyk",
            });
            let n = c.space.components();
            for v in c.components.iter().take(n) {
                let _ = write!(self.out, "\\c{}", (v * 100_000.0).round() as i64);
            }
            if c.alpha() < 1.0 {
                let _ = write!(self.out, "\\c{}", (c.alpha() * 100_000.0).round() as i64);
            }
            if let Some(name) = &c.name {
                let _ = write!(self.out, "\\cname {name}");
            }
            self.out.push(';');
        }
        self.colors = colors;
        self.out.push_str("}\n");
        self.info(attrs);
    }

    fn info(&mut self, a: &DocAttrs) {
        let fields: [(&str, Option<&str>); 9] = [
            ("\\title", a.title.as_deref()),
            ("\\author", a.author.as_deref()),
            ("\\subject", a.subject.as_deref()),
            ("\\doccomm", a.comment.as_deref()),
            ("\\operator", a.editor.as_deref()),
            ("\\*\\manager", a.manager.as_deref()),
            ("\\*\\company", a.company.as_deref()),
            ("\\*\\copyright", a.copyright.as_deref()),
            ("\\*\\category", a.category.as_deref()),
        ];
        let keywords = a.keywords.join(", ");
        let any = fields.iter().any(|(_, v)| v.is_some()) || !keywords.is_empty();
        if any {
            self.out.push_str("{\\info");
            for (word, value) in fields {
                if let Some(v) = value {
                    let _ = write!(self.out, "\n{{{word} ");
                    self.text(v);
                    self.out.push('}');
                    self.uc0 = false;
                }
            }
            if !keywords.is_empty() {
                self.out.push_str("\n{\\keywords ");
                self.text(&keywords);
                self.out.push('}');
                self.uc0 = false;
            }
            self.out.push('}');
        }
        let mut page = String::new();
        if let Some((w, h)) = a.paper_size
            && (w, h) != PAPER
        {
            let _ = write!(page, "\\paperw{}\\paperh{}", twips(w), twips(h));
        }
        for (word, v) in
            [("margl", a.left_margin), ("margr", a.right_margin), ("margb", a.bottom_margin), ("margt", a.top_margin)]
        {
            if let Some(v) = v {
                let _ = write!(page, "\\{word}{}", twips(v));
            }
        }
        if let Some((w, h)) = a.view_size {
            let _ = write!(page, "\\vieww{}\\viewh{}", twips(w), twips(h));
        }
        if let Some(z) = a.view_zoom {
            let _ = write!(page, "\\viewscale{}", z.round() as i64);
        }
        if let Some(k) = a.view_mode {
            let _ = write!(page, "\\viewkind{k}");
        }
        if !page.is_empty() {
            self.out.push_str(&page);
            self.out.push('\n');
        }
        if let Some(f) = a.hyphenation_factor
            && f > 0.0
        {
            let _ = writeln!(self.out, "\\hyphauto1\\hyphfactor{}", (f * 100.0).round() as i64);
        }
        if let Some(t) = a.default_tab_interval
            && t > 0.0
        {
            let _ = writeln!(self.out, "\\deftab{}", twips(t));
        }
    }

    fn body(&mut self, doc: &Doc) {
        let mut written = Written::start();
        let mut previous: Option<Option<&ParaStyle>> = None;
        let runs: Vec<_> = doc.run_ranges().collect();
        let mut run = 0;
        let text_width = doc.attrs.text_width();
        let default_para = ParaStyle::default();
        for (range, para) in doc.para_ranges() {
            let first = previous.is_none();
            if previous != Some(para) {
                let style = para.unwrap_or(&default_para);
                self.out.push_str("\\pard");
                self.paragraph(style, text_width);
                self.out.push('\n');
                if first {
                    self.out.push('\n');
                } else {
                    // The next run's color is written again after each
                    // `\pard`, as AppKit does.
                    written.color = usize::MAX;
                }
                previous = Some(para);
            }
            // The paragraph's text, run by run, links as fields; its
            // separator as a paragraph end.
            let body_end = paragraph_body_end(&doc.text, range.clone());
            let mut at = range.start;
            while at < body_end {
                while runs[run].0.end <= at {
                    run += 1;
                }
                let style = runs[run].1;
                if let Some(link) = &style.link {
                    // The runs with this link, together.
                    let mut end = runs[run].0.end.min(body_end);
                    let mut next = run + 1;
                    while end < body_end && next < runs.len() && runs[next].1.link.as_ref() == Some(link) {
                        end = runs[next].0.end.min(body_end);
                        next += 1;
                    }
                    self.out.push_str("{\\field{\\*\\fldinst{HYPERLINK \"");
                    let uc0 = self.uc0;
                    self.url(link);
                    self.out.push_str("\"}}{\\fldrslt ");
                    // The instruction's groups ended, and a `\uc0` in them.
                    self.uc0 = uc0;
                    let before = (written.clone(), self.uc0);
                    let mut inner = written.clone();
                    let mut i = run;
                    let mut pos = at;
                    while pos < end {
                        let r = &runs[i];
                        let stop = r.0.end.min(end);
                        self.format(r.1, &mut inner);
                        self.text(&doc.text[pos..stop]);
                        pos = stop;
                        i += 1;
                    }
                    self.out.push_str("}}");
                    (written, self.uc0) = before;
                    at = end;
                } else {
                    let stop = runs[run].0.end.min(body_end);
                    self.format(style, &mut written);
                    self.text(&doc.text[at..stop]);
                    at = stop;
                }
            }
            if body_end < range.end {
                // The separator in its own formatting (an empty line's
                // height comes from it). The runs cover the text, so one
                // holds it.
                while runs[run].0.end <= body_end {
                    run += 1;
                }
                self.format(runs[run].1, &mut written);
                self.out.push_str("\\\n");
            }
        }
    }

    /// Paragraph formatting after `\pard`, in AppKit's order.
    fn paragraph(&mut self, p: &ParaStyle, text_width: f64) {
        for tab in &p.tabs {
            self.out.push_str(match tab.kind {
                TabKind::Left => "",
                TabKind::Right => "\\tqr",
                TabKind::Center => "\\tqc",
                TabKind::Decimal => "\\tqdec",
            });
            let _ = write!(self.out, "\\tx{}", twips(tab.location));
        }
        if p.head_indent != 0.0 {
            let _ = write!(self.out, "\\li{}", twips(p.head_indent));
        }
        if p.first_line_head_indent != p.head_indent {
            let _ = write!(self.out, "\\fi{}", twips(p.first_line_head_indent - p.head_indent));
        }
        if p.tail_indent != 0.0 {
            // A tail indent from the leading margin is the right indent it
            // leaves on the page.
            let right = if p.tail_indent < 0.0 { -p.tail_indent } else { text_width - p.tail_indent };
            let _ = write!(self.out, "\\ri{}", twips(right));
        }
        if p.line_height_multiple > 0.0 {
            let _ = write!(self.out, "\\sl{}\\slmult1", (p.line_height_multiple * 240.0).round() as i64);
            if p.minimum_line_height > 0.0 {
                let _ = write!(self.out, "\\slminimum{}", twips(p.minimum_line_height));
            }
            if p.maximum_line_height > 0.0 {
                let _ = write!(self.out, "\\slmaximum{}", twips(p.maximum_line_height));
            }
        } else if p.minimum_line_height > 0.0 && p.minimum_line_height == p.maximum_line_height {
            let _ = write!(self.out, "\\sl-{}", twips(p.minimum_line_height));
        } else {
            if p.minimum_line_height > 0.0 {
                let _ = write!(self.out, "\\slminimum{}", twips(p.minimum_line_height));
            }
            if p.maximum_line_height > 0.0 {
                let _ = write!(self.out, "\\slmaximum{}", twips(p.maximum_line_height));
            }
        }
        if p.line_spacing != 0.0 {
            let _ = write!(self.out, "\\slleading{}", twips(p.line_spacing));
        }
        if p.paragraph_spacing_before != 0.0 {
            let _ = write!(self.out, "\\sb{}", twips(p.paragraph_spacing_before));
        }
        if p.paragraph_spacing != 0.0 {
            let _ = write!(self.out, "\\sa{}", twips(p.paragraph_spacing));
        }
        if p.default_tab_interval > 0.0 {
            let _ = write!(self.out, "\\pardeftab{}", twips(p.default_tab_interval));
        }
        match p.direction {
            Direction::Natural => self.out.push_str("\\pardirnatural"),
            Direction::RightToLeft => self.out.push_str("\\rtlpar"),
            Direction::LeftToRight => {}
        }
        let rtl = p.direction == Direction::RightToLeft;
        self.out.push_str(match p.alignment {
            Align::Center => "\\qc",
            Align::Justified => "\\qj",
            Align::Right => "\\qr",
            // Natural alignment runs the paragraph's way.
            Align::Natural if rtl => "\\qr",
            Align::Left if rtl => "\\ql",
            _ => "",
        });
        self.out.push_str("\\partightenfactor0");
    }

    /// Write the control words that take `w` to `style`.
    fn format(&mut self, style: &CharStyle, w: &mut Written) {
        let mut words = String::new();
        // The font: a change of face on a line of its own.
        let f = font_of(style);
        let (font, size, bold, italic) = (Some(self.font_index(&f)), f.size, f.bold, f.italic);
        let start = w.font.is_none();
        if font != w.font {
            if !start {
                words.push('\n');
            }
            let _ = write!(words, "\\f{}", font.unwrap_or(0));
        }
        if italic != w.italic {
            words.push_str(if italic { "\\i" } else { "\\i0" });
        }
        if bold != w.bold {
            words.push_str(if bold { "\\b" } else { "\\b0" });
        }
        if size != w.size || start {
            let half = (size * 2.0).floor() as i64;
            let _ = write!(words, "\\fs{half}");
            if (size * 2.0).fract() != 0.0 {
                let _ = write!(words, "\\fsmilli{}", (size * 1000.0).round() as i64);
            }
        }
        (w.font, w.size, w.bold, w.italic) = (font, size, bold, italic);
        if !words.is_empty() {
            words.push(' ');
        }
        let color = style.color.as_ref().map_or(0, |c| self.color_index(c));
        if color != w.color {
            let _ = write!(words, "\\cf{color} ");
            w.color = color;
        }
        let background = style.background.as_ref().map_or(1, |c| self.color_index(c));
        if background != w.background {
            let _ = write!(words, "\\cb{background} ");
            w.background = background;
        }
        let underline_color = style.underline_color.as_ref().map_or(0, |c| self.color_index(c));
        if style.underline != w.underline || underline_color != w.underline_color {
            words.push_str(&underline(style.underline));
            if style.underline != 0 && !(style.underline == 1 && underline_color == 0) {
                let _ = write!(words, "\\ulc{underline_color} ");
            }
            (w.underline, w.underline_color) = (style.underline, underline_color);
        }
        let strike_color = style.strikethrough_color.as_ref().map_or(0, |c| self.color_index(c));
        if style.strikethrough != w.strikethrough || strike_color != w.strikethrough_color {
            let s = style.strikethrough;
            match s {
                0 => words.push_str("\\strike0\\striked0 "),
                1 => words.push_str("\\strike "),
                9 => words.push_str("\\striked1 "),
                _ => {
                    let _ = write!(words, "\\strike \\strikestyle{s} ");
                }
            }
            if s != 0 {
                let _ = write!(words, "\\strikec{strike_color} ");
            }
            (w.strikethrough, w.strikethrough_color) = (s, strike_color);
        }
        let baseline = (style.baseline_offset * 2.0).round() as i64;
        if baseline != w.baseline {
            match baseline {
                b if b < 0 => {
                    let _ = write!(words, "\\dn{} ", -b);
                }
                b => {
                    let _ = write!(words, "\\up{b} ");
                }
            }
            w.baseline = baseline;
        }
        if style.superscript != w.superscript {
            match style.superscript {
                0 => words.push_str("\\nosupersub "),
                1 => words.push_str("\\super "),
                -1 => words.push_str("\\sub "),
                s if s > 0 => {
                    let _ = write!(words, "\\super{s} ");
                }
                s => {
                    let _ = write!(words, "\\sub{} ", -s);
                }
            }
            w.superscript = style.superscript;
        }
        let kern = style.kern.map(|k| ((k * 4.0).round() as i64, twips(k)));
        if kern != w.kern {
            let (q, t) = kern.unwrap_or((0, 0));
            let _ = writeln!(words, "\\kerning1\\expnd{q}\\expndtw{t}");
            w.kern = kern;
        }
        let shadow = style.shadow.as_ref().map(|s| {
            let color = s.color.as_ref().map_or(0, |c| self.color_index(&opaque(c)));
            let alpha = s.color.as_ref().map_or(0.0, Color::alpha);
            (twips(s.offset.0), twips(s.offset.1), twips(s.blur), (alpha * 255.0).round() as i64, color)
        });
        if shadow != w.shadow {
            match shadow {
                Some((x, y, r, o, c)) => {
                    let _ = write!(words, "\\shad\\shadx{x}\\shady{y}\\shadr{r}\\shado{o} \\shadc{c} ");
                }
                None => words.push_str("\\shad0 "),
            }
            w.shadow = shadow;
        }
        let expansion = (style.expansion * 2000.0).round() as i64;
        if expansion != w.expansion {
            let _ = write!(words, "\\expansion{expansion} ");
            w.expansion = expansion;
        }
        let obliqueness = (style.obliqueness * 2000.0).round() as i64;
        if obliqueness != w.obliqueness {
            let _ = write!(words, "\\obliqueness{obliqueness} ");
            w.obliqueness = obliqueness;
        }
        let stroke = (style.stroke_width != 0.0)
            .then(|| (twips(style.stroke_width), style.stroke_color.as_ref().map_or(0, |c| self.color_index(c))));
        if stroke != w.stroke {
            match stroke {
                Some((width, color)) => {
                    let outline = if width > 0 { "\\outl" } else { "\\outl0" };
                    let _ = write!(words, "{outline}\\strokewidth{width} \\strokec{color} ");
                }
                None => words.push_str("\\outl0\\strokewidth0 "),
            }
            w.stroke = stroke;
        }
        if style.ligature != w.ligature {
            let _ = write!(words, "\\CocoaLigature{} ", style.ligature.unwrap_or(1));
            w.ligature = style.ligature;
        }
        self.out.push_str(&words);
    }

    /// Text, escaped: RTF's specials, characters beyond ASCII in
    /// Windows-1252 or as `\uN`.
    fn text(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\\' => self.out.push_str("\\\\"),
                '{' => self.out.push_str("\\{"),
                '}' => self.out.push_str("\\}"),
                '\t' => self.out.push('\t'),
                '\n' | '\r' | '\u{2029}' => self.out.push_str("\\\n"),
                ' '..='~' => self.out.push(c),
                _ => match CodePage::Cp1252.encode(c).filter(|&b| b >= 0x80) {
                    Some(b) => {
                        let _ = write!(self.out, "\\'{b:02x}");
                    }
                    None => {
                        if !self.uc0 {
                            self.out.push_str("\\uc0");
                            self.uc0 = true;
                        }
                        let mut units = [0u16; 2];
                        for u in c.encode_utf16(&mut units) {
                            let _ = write!(self.out, "\\u{u} ");
                        }
                    }
                },
            }
        }
    }

    /// A link's target, in a field instruction's quotes: RTF's specials
    /// escaped, and spaces and quotes as URLs have them.
    fn url(&mut self, url: &str) {
        let mut escaped = String::with_capacity(url.len());
        for c in url.chars() {
            match c {
                ' ' => escaped.push_str("%20"),
                '"' => escaped.push_str("%22"),
                _ => escaped.push(c),
            }
        }
        self.text(&escaped);
    }
}

/// The control words for an underline style.
fn underline(style: i64) -> String {
    if style == 0 {
        return "\\ulnone ".into();
    }
    if style & 0x8000 != 0 {
        return "\\ulw ".into();
    }
    let thick = style & 0xf == 2;
    let word = match (style & 0xf, style & 0xf00) {
        (9, _) => "\\uldb",
        (_, 0x100) if thick => "\\ulthd",
        (_, 0x200) if thick => "\\ulthdash",
        (_, 0x300) if thick => "\\ulthdashd",
        (_, 0x400) if thick => "\\ulthdashdd",
        (_, 0x100) => "\\uld",
        (_, 0x200) => "\\uldash",
        (_, 0x300) => "\\uldashd",
        (_, 0x400) => "\\uldashdd",
        _ if thick => "\\ulth",
        _ => "\\ul",
    };
    format!("{word} ")
}

/// A color without its alpha, which a shadow writes apart (`\shado`).
fn opaque(c: &Color) -> Color {
    let mut c = c.clone();
    if let Some(a) = c.components.last_mut() {
        *a = 1.0;
    }
    c.srgb[3] = 1.0;
    c
}

/// Where the text of the paragraph `range` ends: before its separator.
fn paragraph_body_end(text: &str, range: std::ops::Range<usize>) -> usize {
    let t = &text[range.clone()];
    let sep = if t.ends_with("\r\n") {
        2
    } else if t.ends_with('\n') || t.ends_with('\r') {
        1
    } else if t.ends_with('\u{2029}') {
        3
    } else {
        0
    };
    range.end - sep
}

#[cfg(test)]
mod tests {
    use super::super::model::{Builder, Shadow, Tab};
    use super::super::rtf_read;
    use super::*;

    fn helvetica(bold: bool) -> Font {
        Font {
            names: vec![if bold { "Helvetica-Bold" } else { "Helvetica" }.into()],
            family: "Helvetica".into(),
            generic: Generic::Sans,
            size: 12.0,
            bold,
            italic: false,
        }
    }

    fn plain() -> CharStyle {
        CharStyle { font: Some(helvetica(false)), ..CharStyle::default() }
    }

    fn rtf(doc: &Doc) -> String {
        String::from_utf8(write(doc)).unwrap()
    }

    const TABS: &str =
        "\\tx560\\tx1120\\tx1680\\tx2240\\tx2800\\tx3360\\tx3920\\tx4480\\tx5040\\tx5600\\tx6160\\tx6720";

    #[test]
    fn plain_text_is_written_as_appkit_writes_it() {
        let mut b = Builder::new();
        b.push("plain no attrs", &plain());
        let doc = b.finish(None);
        let expected = format!(
            "{{\\rtf1\\ansi\\ansicpg1252\\cocoartf2870\n\\cocoatextscaling0\\cocoaplatform0{{\\fonttbl\\f0\\fswiss\\fcharset0 Helvetica;}}\n{{\\colortbl;\\red255\\green255\\blue255;}}\n{{\\*\\expandedcolortbl;;}}\n\\pard{TABS}\\pardirnatural\\partightenfactor0\n\n\\f0\\fs24 \\cf0 plain no attrs}}"
        );
        assert_eq!(rtf(&doc), expected);
    }

    #[test]
    fn fonts_colors_and_paragraphs() {
        let mut b = Builder::new();
        let bold = CharStyle { font: Some(helvetica(true)), ..plain() };
        let red = CharStyle { color: Some(Color::srgb(1.0, 0.0, 0.0, 1.0)), ..plain() };
        let centered = ParaStyle { alignment: Align::Center, ..ParaStyle::default() };
        b.push("Normal ", &plain());
        b.push("Bold", &bold);
        b.push(" ", &plain());
        b.push("red", &red);
        b.end_paragraph("\n", &plain(), Some(&centered));
        b.push("next", &plain());
        let doc = b.finish(None);
        let out = rtf(&doc);
        assert!(
            out.contains("{\\fonttbl\\f0\\fswiss\\fcharset0 Helvetica;\\f1\\fswiss\\fcharset0 Helvetica-Bold;}"),
            "{out}"
        );
        assert!(out.contains("{\\colortbl;\\red255\\green255\\blue255;\\red255\\green0\\blue0;}"), "{out}");
        assert!(out.contains("{\\*\\expandedcolortbl;;\\cssrgb\\c100000\\c0\\c0;}"), "{out}");
        assert!(out.contains("\\pardirnatural\\qc\\partightenfactor0\n\n\\f0\\fs24 \\cf0 Normal \n\\f1\\b Bold\n\\f0\\b0  \\cf2 red\\cf0 \\\n\\pard"), "{out}");
        // Read back, it is the same text in the same styles.
        let back = rtf_read::read(out.as_bytes()).unwrap();
        assert_eq!(back.text, doc.text);
        let fonts: Vec<_> =
            back.run_ranges().map(|(r, s)| (&back.text[r], s.font.as_ref().unwrap().names[0].clone())).collect();
        assert_eq!(fonts[1], ("Bold", "Helvetica-Bold".to_string()));
        assert_eq!(back.paras[0].1.as_ref().unwrap().alignment, Align::Center);
    }

    #[test]
    fn text_is_escaped() {
        let mut b = Builder::new();
        b.push("café € “quotes” — 日本 😀 \\ { } tab\there\u{2028}x", &plain());
        let out = rtf(&b.finish(None));
        assert!(
            out.contains("caf\\'e9 \\'80 \\'93quotes\\'94 \\'97 \\uc0\\u26085 \\u26412  \\u55357 \\u56832  \\\\ \\{ \\} tab\there\\u8232 x}"),
            "{out}"
        );
        let back = rtf_read::read(out.as_bytes()).unwrap();
        assert_eq!(back.text, "café € “quotes” — 日本 😀 \\ { } tab\there\u{2028}x");
    }

    #[test]
    fn character_formatting_round_trips() {
        let blue = Color::srgb(0.0, 0.0, 1.0, 1.0);
        let styles = [
            CharStyle { underline: 9, underline_color: Some(blue.clone()), ..plain() },
            CharStyle { underline: 1 | 0x8000, ..plain() },
            CharStyle { strikethrough: 1, strikethrough_color: Some(blue.clone()), ..plain() },
            CharStyle { baseline_offset: 3.0, superscript: 1, ..plain() },
            CharStyle { superscript: -1, ..plain() },
            CharStyle { kern: Some(1.5), ..plain() },
            CharStyle {
                shadow: Some(Shadow { offset: (2.0, -2.0), blur: 3.0, color: Some(Color::srgb(1.0, 0.0, 0.0, 0.5)) }),
                ..plain()
            },
            CharStyle { expansion: 0.2, obliqueness: -0.25, ..plain() },
            CharStyle { stroke_width: -3.0, stroke_color: Some(blue.clone()), ..plain() },
            CharStyle { background: Some(blue.clone()), ..plain() },
            CharStyle { font: Some(Font { size: 13.3, ..helvetica(false) }), ..plain() },
            CharStyle { link: Some("https://example.com/a b".into()), ..plain() },
        ];
        let mut b = Builder::new();
        for (i, s) in styles.iter().enumerate() {
            b.push(&format!("{i}"), s);
            b.push(" ", &plain());
        }
        let doc = b.finish(None);
        let out = rtf(&doc);
        let back = rtf_read::read(out.as_bytes()).unwrap();
        assert_eq!(back.text, doc.text);
        let got: Vec<CharStyle> =
            back.run_ranges().filter(|(r, _)| back.text[r.clone()].trim() != "").map(|(_, s)| s.clone()).collect();
        assert_eq!(got.len(), styles.len(), "{out}");
        for (i, (want, got)) in styles.iter().zip(&got).enumerate() {
            assert_eq!(got.underline, want.underline, "{i}");
            assert_eq!(
                got.underline_color.as_ref().map(|c| c.srgb),
                want.underline_color.as_ref().map(|c| c.srgb),
                "{i}"
            );
            assert_eq!(got.strikethrough, want.strikethrough, "{i}");
            assert_eq!(
                (got.baseline_offset, got.superscript, got.kern),
                (want.baseline_offset, want.superscript, want.kern),
                "{i}"
            );
            assert_eq!(
                (got.expansion, got.obliqueness, got.stroke_width),
                (want.expansion, want.obliqueness, want.stroke_width),
                "{i}"
            );
            assert_eq!(got.background.as_ref().map(|c| c.srgb), want.background.as_ref().map(|c| c.srgb), "{i}");
            assert_eq!(got.font.as_ref().unwrap().size, want.font.as_ref().unwrap().size, "{i}");
            assert_eq!(got.link.as_deref(), want.link.as_deref().map(|l| l.replace(' ', "%20")).as_deref(), "{i}");
        }
        let shadow = got[6].shadow.as_ref().unwrap();
        assert_eq!((shadow.offset, shadow.blur), ((2.0, -2.0), 3.0));
        assert!((shadow.color.as_ref().unwrap().alpha() - 0.5).abs() < 0.01);
    }

    /// After each `\pard` the next run's color is written once, whether or
    /// not it changes, as AppKit writes it (bytes measured on macOS).
    #[test]
    fn colors_after_paragraph_changes() {
        let right = ParaStyle { alignment: Align::Right, ..ParaStyle::default() };
        let red = Color::srgb(1.0, 0.0, 0.0, 1.0);
        let second = |style: CharStyle, first: CharStyle| {
            let mut b = Builder::new();
            b.push("one", &first);
            b.end_paragraph("\n", &first, None);
            b.push("two", &style);
            let out = rtf(&b.finish(Some(&right)));
            out[out.rfind("\\partightenfactor0\n").unwrap() + 19..].to_string()
        };
        assert_eq!(second(plain(), plain()), "\\cf0 two}");
        assert_eq!(second(CharStyle { color: Some(red.clone()), ..plain() }, plain()), "\\cf2 two}");
        assert_eq!(second(CharStyle { font: Some(helvetica(true)), ..plain() }, plain()), "\n\\f1\\b \\cf0 two}");
        let red_style = CharStyle { color: Some(red), ..plain() };
        assert_eq!(second(red_style.clone(), red_style.clone()), "\\cf2 two}");
        assert_eq!(second(plain(), red_style), "\\cf0 two}");
    }

    #[test]
    fn paragraph_formatting_round_trips() {
        let styles = [
            ParaStyle { alignment: Align::Right, ..ParaStyle::default() },
            ParaStyle { first_line_head_indent: 36.0, head_indent: 18.0, tail_indent: -20.0, ..ParaStyle::default() },
            ParaStyle {
                paragraph_spacing: 10.0,
                paragraph_spacing_before: 5.0,
                line_spacing: 3.0,
                minimum_line_height: 15.0,
                maximum_line_height: 30.0,
                line_height_multiple: 1.5,
                ..ParaStyle::default()
            },
            ParaStyle {
                tabs: vec![
                    Tab { location: 50.0, kind: TabKind::Left },
                    Tab { location: 150.0, kind: TabKind::Right },
                    Tab { location: 200.0, kind: TabKind::Center },
                    Tab { location: 250.0, kind: TabKind::Decimal },
                ],
                default_tab_interval: 40.0,
                ..ParaStyle::default()
            },
            ParaStyle { minimum_line_height: 20.0, maximum_line_height: 20.0, ..ParaStyle::default() },
            ParaStyle { direction: Direction::RightToLeft, ..ParaStyle::default() },
            ParaStyle { tail_indent: 412.0, ..ParaStyle::default() },
        ];
        let mut b = Builder::new();
        for (i, s) in styles.iter().enumerate() {
            b.push(&format!("p{i}"), &plain());
            b.end_paragraph("\n", &plain(), Some(s));
        }
        let doc = b.finish(None);
        let out = rtf(&doc);
        assert!(
            out.contains(&format!("\\pard{TABS}\\li360\\fi360\\ri400\\pardirnatural\\partightenfactor0\n\\cf0 p1")),
            "{out}"
        );
        assert!(
            out.contains("\\pard\\tx1000\\tqr\\tx3000\\tqc\\tx4000\\tqdec\\tx5000\\pardeftab800\\pardirnatural"),
            "{out}"
        );
        assert!(out.contains("\\sl-400"), "{out}");
        assert!(out.contains(&format!("\\pard{TABS}\\rtlpar\\qr\\partightenfactor0")), "{out}");
        let back = rtf_read::read(out.as_bytes()).unwrap();
        let got: Vec<ParaStyle> = back.paras.iter().filter_map(|p| p.1.clone()).collect();
        assert_eq!(got.len(), styles.len());
        for (want, got) in styles.iter().zip(&got) {
            // RTF has no natural alignment but in right-to-left text.
            let align = if want.alignment == Align::Natural && want.direction != Direction::RightToLeft {
                Align::Left
            } else if want.alignment == Align::Natural {
                Align::Right
            } else {
                want.alignment
            };
            assert_eq!(got.alignment, align);
            assert_eq!((got.first_line_head_indent, got.head_indent), (want.first_line_head_indent, want.head_indent));
            // Tail indents come back from the leading margin.
            let tail = if want.tail_indent < 0.0 { 432.0 + want.tail_indent } else { want.tail_indent };
            assert_eq!(got.tail_indent, tail);
            assert_eq!(
                (got.paragraph_spacing, got.paragraph_spacing_before, got.line_spacing),
                (want.paragraph_spacing, want.paragraph_spacing_before, want.line_spacing)
            );
            assert_eq!(
                (got.minimum_line_height, got.maximum_line_height, got.line_height_multiple),
                (want.minimum_line_height, want.maximum_line_height, want.line_height_multiple)
            );
            assert_eq!(got.tabs, want.tabs);
            assert_eq!(got.default_tab_interval, want.default_tab_interval);
            assert_eq!(got.direction, want.direction);
        }
    }

    #[test]
    fn documents_are_described() {
        let mut b = Builder::new();
        b.push("doc", &plain());
        let mut doc = b.finish(None);
        doc.attrs = DocAttrs {
            title: Some("T".into()),
            author: Some("A".into()),
            keywords: vec!["k1".into(), "k2".into()],
            read_only: Some(1),
            paper_size: Some((500.0, 700.0)),
            left_margin: Some(50.0),
            view_mode: Some(1),
            hyphenation_factor: Some(0.5),
            default_tab_interval: Some(30.0),
            ..DocAttrs::default()
        };
        let out = rtf(&doc);
        assert!(out.contains("\n\\readonlydoc1\\cocoatextscaling0"), "{out}");
        assert!(out.contains("{\\info\n{\\title T}\n{\\author A}\n{\\keywords k1, k2}}\\paperw10000\\paperh14000\\margl1000\\viewkind1\n\\hyphauto1\\hyphfactor50\n\\deftab600\n"), "{out}");
        let back = rtf_read::read(out.as_bytes()).unwrap();
        assert_eq!(back.attrs.title.as_deref(), Some("T"));
        assert_eq!(back.attrs.keywords, ["k1", "k2"]);
        assert_eq!((back.attrs.paper_size, back.attrs.read_only), (Some((500.0, 700.0)), Some(1)));
        // An empty document is a header alone.
        let empty = rtf(&Builder::new().finish(None));
        assert!(
            empty.ends_with("{\\fonttbl}\n{\\colortbl;\\red255\\green255\\blue255;}\n{\\*\\expandedcolortbl;;}\n}"),
            "{empty}"
        );
        assert_eq!(rtf_read::read(empty.as_bytes()).unwrap().text, "");
    }
}
