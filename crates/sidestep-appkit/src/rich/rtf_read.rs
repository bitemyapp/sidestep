//! Reading RTF into a [`Doc`].
//!
//! Written from Microsoft's published RTF specification (1.9.1) and from
//! what AppKit writes and reads on macOS (Cocoa's own control words:
//! `\cocoartf`, the extended color table, `\slleading`, `\expansion`,
//! `\CocoaLigature` and the rest). What AppKit makes of the common
//! constructs, measured there, is followed where a program can see it:
//!
//! - A paragraph takes the paragraph formatting in effect at its end.
//!   Paragraph formatting makes a paragraph style only once some control
//!   word sets it (`\pard` among them); text with none has no style. After
//!   `\pard` a paragraph is left-aligned, left to right, with no tab stops
//!   and the document's default tab interval (`\deftab`, else 36 points,
//!   or 0 in Cocoa's RTF).
//! - A right indent (`\ri`) becomes a tail indent measured from the
//!   leading margin: the page's text width (paper less margins) less the
//!   indent.
//! - Fonts are found by the font table's name, a PostScript or family
//!   name; a name nobody has falls back by its family class (`\froman` to
//!   Times, `\fmodern` to Courier, the rest to Helvetica). Text before any
//!   `\f`, or in a font the table lacks, is Helvetica; `\plain` goes back
//!   to `\f0`, and `\deff` is ignored. `\b` and `\i` add their traits to
//!   the font; turning them off doesn't take away a font's own. `\fs0` and
//!   negative sizes are ignored.
//! - A parameter keeps the low 32 bits of all its digits.
//! - Colors come from Cocoa's extended color table where it has the entry
//!   (`\cssrgb`, `\csgenericrgb`, `\cspthree`, `\csgray`, `\cscmyk`,
//!   `\cname`), else from `\red\green\blue` as calibrated RGB. `\cf0` is no
//!   color; in Cocoa's RTF, `\cb1` (the white it always puts first) is no
//!   background.
//! - `\line` is U+2028 and `\page` a form feed; `\sect` is nothing; `\-`
//!   (an optional hyphen) is dropped. `\uN` takes `\ucN` characters after
//!   it as its stand-ins, and surrogate pairs join.
//! - Unknown control words are skipped, and groups that start with `\*`
//!   and an unknown word are skipped whole, as the specification says.
//! - Data that isn't RTF is an error, and so is RTF that ends before its
//!   outermost group does.
//!
//! Where Sidestep reads differently from AppKit: Word's `\highlight` and
//! `\chcbpat` are backgrounds, and `\cb0` is none (AppKit ignores the first
//! two and reads the third as black); table cells and rows are tabs and
//! paragraph ends; a hyperlink field's instruction is parsed (its quoted
//! target, and `\l` for an anchor, `#anchor`); `\expnd` is in quarter
//! points, as the specification has it; `\bin` data is skipped. What it
//! doesn't read: pictures and objects (skipped; attachments come with
//! `NSTextAttachment`), and double-byte code pages (Shift-JIS, GBK, Big5,
//! …): text in them reads as the document's single-byte page, though the
//! `\u` escapes writers put beside it read rightly.

use super::model::{
    Align, Builder, CharStyle, Color, Direction, Doc, DocAttrs, Font, Generic, ParaStyle, Shadow, Space, Tab, TabKind,
};
use super::tables::CodePage;

/// Why RTF couldn't be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    /// The data isn't RTF at all.
    NotRtf,
    /// It starts as RTF but ends before its outermost group does.
    Truncated,
}

/// Whether `data` starts as RTF does.
pub(crate) fn is_rtf(data: &[u8]) -> bool {
    data.starts_with(b"{\\rtf")
}

/// Read `data` as RTF.
#[cfg(test)]
pub(crate) fn read(data: &[u8]) -> Result<Doc, Error> {
    read_with(data, &[])
}

/// Read `data` as the RTF of an RTFD package holding `files`: each
/// attachment (`\NeXTGraphic`) whose file the package has becomes a U+FFFC
/// and an attachment of the document; one whose file it lacks (in plain
/// RTF, all of them) is left out.
pub(crate) fn read_with(data: &[u8], files: &[(String, Vec<u8>)]) -> Result<Doc, Error> {
    let start = data.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(data.len());
    if !is_rtf(&data[start..]) {
        return Err(Error::NotRtf);
    }
    let mut r = Reader::new(&data[start..], files);
    r.run()?;
    Ok(r.finish())
}

// Tokens.

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token<'a> {
    Open,
    Close,
    /// A control word and its parameter.
    Word(&'a [u8], Option<i32>),
    /// A control symbol: `\` and one character that isn't a letter.
    Symbol(u8),
    /// `\'hh`: a byte of text.
    Hex(u8),
    /// Text: bytes up to the next `\`, `{`, `}` or line end.
    Text(&'a [u8]),
    /// `\binN` and its bytes, skipped.
    Binary,
}

struct Lexer<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Lexer<'a> {
    fn next(&mut self) -> Option<Token<'a>> {
        loop {
            let b = *self.data.get(self.at)?;
            match b {
                b'{' => {
                    self.at += 1;
                    return Some(Token::Open);
                }
                b'}' => {
                    self.at += 1;
                    return Some(Token::Close);
                }
                // Line ends in RTF are for the writer's convenience.
                b'\r' | b'\n' => self.at += 1,
                b'\\' => return Some(self.control()),
                _ => {
                    let start = self.at;
                    while self.at < self.data.len()
                        && !matches!(self.data[self.at], b'{' | b'}' | b'\\' | b'\r' | b'\n')
                    {
                        self.at += 1;
                    }
                    return Some(Token::Text(&self.data[start..self.at]));
                }
            }
        }
    }

    /// A control word or symbol, `self.at` at its backslash.
    fn control(&mut self) -> Token<'a> {
        self.at += 1;
        let Some(&c) = self.data.get(self.at) else { return Token::Symbol(b'\\') };
        if !c.is_ascii_alphabetic() {
            self.at += 1;
            if c == b'\'' {
                let hex = |b: Option<&u8>| b.and_then(|b| char::from(*b).to_digit(16));
                if let (Some(h), Some(l)) = (hex(self.data.get(self.at)), hex(self.data.get(self.at + 1))) {
                    self.at += 2;
                    return Token::Hex((h * 16 + l) as u8);
                }
            }
            return Token::Symbol(c);
        }
        let start = self.at;
        while self.at < self.data.len() && self.data[self.at].is_ascii_alphabetic() && self.at - start < 32 {
            self.at += 1;
        }
        let word = &self.data[start..self.at];
        let negative =
            self.data.get(self.at) == Some(&b'-') && self.data.get(self.at + 1).is_some_and(u8::is_ascii_digit);
        if negative {
            self.at += 1;
        }
        // Every digit is the parameter's, and the number keeps its low 32
        // bits, as AppKit reads it (`\li99999999999` is `\li1215752191`).
        let digits = self.at;
        let mut n = 0i64;
        while let Some(d) = self.data.get(self.at).filter(|b| b.is_ascii_digit()) {
            n = n.wrapping_mul(10).wrapping_add(i64::from(d - b'0'));
            self.at += 1;
        }
        let param = (self.at > digits).then(|| (if negative { n.wrapping_neg() } else { n }) as i32);
        // A space delimits the word and is part of it.
        if self.data.get(self.at) == Some(&b' ') {
            self.at += 1;
        }
        if word == b"bin" {
            let n = param.unwrap_or(0).max(0) as usize;
            self.at = self.at.saturating_add(n).min(self.data.len());
            return Token::Binary;
        }
        Token::Word(word, param)
    }
}

// State.

/// Where text goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dest {
    Text,
    FontTable,
    ColorTable,
    ExpandedColorTable,
    Info,
    InfoField(InfoField),
    /// A field's instruction, to the field of this index.
    FieldInstruction(usize),
    /// An attachment's graphic (RTFD's `\NeXTGraphic`): its file's name
    /// and size.
    Graphic,
    /// Skipped, with everything in it.
    Skip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InfoField {
    Title,
    Author,
    Subject,
    Keywords,
    Comment,
    Company,
    Copyright,
    Editor,
    Manager,
    Category,
}

/// Character formatting as control words set it: indices into the tables,
/// resolved when text arrives.
#[derive(Clone, Debug, PartialEq)]
struct Chr {
    font: Option<i32>,
    size: f64,
    bold: bool,
    italic: bool,
    underline: i64,
    underline_color: Option<usize>,
    strikethrough: i64,
    strikethrough_color: Option<usize>,
    foreground: usize,
    background: Option<usize>,
    baseline_offset: f64,
    superscript: i64,
    kern: Option<f64>,
    ligature: Option<i64>,
    shadow: bool,
    shadow_x: f64,
    shadow_y: f64,
    shadow_blur: f64,
    shadow_opacity: f64,
    shadow_color: usize,
    expansion: f64,
    obliqueness: f64,
    outline: bool,
    stroke_width: Option<f64>,
    stroke_color: Option<usize>,
    /// A link over the text: the index of its field.
    link: Option<usize>,
}

impl Chr {
    fn plain(font: Option<i32>, link: Option<usize>) -> Chr {
        Chr {
            font,
            size: 12.0,
            bold: false,
            italic: false,
            underline: 0,
            underline_color: None,
            strikethrough: 0,
            strikethrough_color: None,
            foreground: 0,
            background: None,
            baseline_offset: 0.0,
            superscript: 0,
            kern: None,
            ligature: None,
            shadow: false,
            shadow_x: 0.0,
            shadow_y: 0.0,
            shadow_blur: 0.0,
            shadow_opacity: 1.0,
            shadow_color: 0,
            expansion: 0.0,
            obliqueness: 0.0,
            outline: false,
            stroke_width: None,
            stroke_color: None,
            link,
        }
    }
}

/// Paragraph formatting as control words set it, in twips where RTF
/// measures in them.
#[derive(Clone, Debug, PartialEq)]
struct Para {
    /// Whether any control word set paragraph formatting: only then does
    /// text get a paragraph style.
    set: bool,
    alignment: Align,
    left: i32,
    first: i32,
    right: i32,
    before: i32,
    after: i32,
    line: i32,
    line_multiple: bool,
    minimum: Option<i32>,
    maximum: Option<i32>,
    leading: i32,
    default_tab: Option<i32>,
    direction: Direction,
    tabs: Vec<Tab>,
    next_tab: TabKind,
    tightening: Option<bool>,
}

impl Para {
    fn new() -> Para {
        Para {
            set: false,
            alignment: Align::Left,
            left: 0,
            first: 0,
            right: 0,
            before: 0,
            after: 0,
            line: 0,
            line_multiple: false,
            minimum: None,
            maximum: None,
            leading: 0,
            default_tab: None,
            direction: Direction::LeftToRight,
            tabs: Vec::new(),
            next_tab: TabKind::Left,
            tightening: None,
        }
    }
}

#[derive(Clone, Debug)]
struct State {
    dest: Dest,
    chr: Chr,
    para: Para,
    /// `\ucN`: how many characters stand in for each `\u`.
    uc: u32,
    /// The group is a field's: the index of the field.
    field: Option<usize>,
    /// A `\*` was read: an unknown destination word skips the group.
    ignorable: bool,
    /// An attachment's placeholder character (¬) may come next, to drop.
    after_attachment: bool,
}

/// A font table entry.
#[derive(Clone, Debug)]
struct FontEntry {
    number: i32,
    name: String,
    generic: Generic,
    code_page: Option<CodePage>,
}

/// A color table entry being read.
#[derive(Clone, Debug, Default)]
struct ColorEntry {
    rgb: [Option<i32>; 3],
    space: Option<Space>,
    components: Vec<i32>,
    name: String,
    naming: bool,
}

impl ColorEntry {
    /// The entry's color, as `\colortbl` gives it: calibrated RGB, or none
    /// (auto) without components.
    fn plain(&self) -> Option<Color> {
        if self.rgb.iter().all(Option::is_none) {
            return None;
        }
        let c = |i: usize| f64::from(self.rgb[i].unwrap_or(0).clamp(0, 255)) / 255.0;
        Some(Color::in_space(Space::GenericRgb, vec![c(0), c(1), c(2)], 1.0))
    }

    /// The entry's color, as the extended table gives it.
    fn expanded(&self) -> Option<Color> {
        let space = self.space?;
        let n = space.components();
        let value = |i: usize| self.components.get(i).map(|&c| f64::from(c) / 100_000.0);
        let components: Vec<f64> = (0..n).map(|i| value(i).unwrap_or(0.0)).collect();
        let mut color = Color::in_space(space, components, value(n).unwrap_or(1.0));
        let name = self.name.trim();
        if !name.is_empty() {
            color.name = Some(name.to_owned());
        }
        Some(color)
    }
}

struct Reader<'a> {
    lexer: Lexer<'a>,
    stack: Vec<State>,
    out: Builder,
    code_page: CodePage,
    fonts: Vec<FontEntry>,
    /// The font table entry being read.
    font: Option<FontEntry>,
    font_name: Vec<u8>,
    colors: Vec<Option<Color>>,
    expanded: Vec<Option<Color>>,
    color_entry: ColorEntry,
    info: String,
    fields: Vec<String>,
    cocoa: bool,
    attrs: DocAttrs,
    /// Characters still to skip after a `\u`.
    skip: u32,
    /// A high surrogate waiting for its low half.
    high: Option<u16>,
    /// The last formatting resolved, and what it resolved to.
    resolved: Option<(Chr, CharStyle)>,
    /// The paragraph formatting the document ended in.
    last_para: Option<Para>,
    /// The files of the RTFD package being read, by name.
    files: &'a [(String, Vec<u8>)],
    /// The attachment being read: its file's name, width and height (in
    /// twips).
    graphic: (Vec<u8>, Option<i32>, Option<i32>),
    attachments: Vec<super::model::Attachment>,
}

fn twips(n: i32) -> f64 {
    f64::from(n) / 20.0
}

fn flag(param: Option<i32>) -> bool {
    param != Some(0)
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], files: &'a [(String, Vec<u8>)]) -> Reader<'a> {
        Reader {
            lexer: Lexer { data, at: 0 },
            stack: Vec::new(),
            out: Builder::new(),
            code_page: CodePage::Cp1252,
            fonts: Vec::new(),
            font: None,
            font_name: Vec::new(),
            colors: Vec::new(),
            expanded: Vec::new(),
            color_entry: ColorEntry::default(),
            info: String::new(),
            fields: Vec::new(),
            cocoa: false,
            attrs: DocAttrs::default(),
            skip: 0,
            high: None,
            resolved: None,
            last_para: None,
            files,
            graphic: (Vec::new(), None, None),
            attachments: Vec::new(),
        }
    }

    fn state(&mut self) -> &mut State {
        self.stack.last_mut().expect("inside a group")
    }

    fn run(&mut self) -> Result<(), Error> {
        while let Some(token) = self.lexer.next() {
            match token {
                Token::Open => {
                    self.skip = 0;
                    let next = match self.stack.last() {
                        Some(s) => State { ignorable: false, after_attachment: false, ..s.clone() },
                        None => State {
                            dest: Dest::Text,
                            chr: Chr::plain(None, None),
                            para: Para::new(),
                            uc: 1,
                            field: None,
                            ignorable: false,
                            after_attachment: false,
                        },
                    };
                    self.stack.push(next);
                }
                Token::Close => {
                    self.skip = 0;
                    self.close_group();
                    if self.stack.is_empty() {
                        return Ok(());
                    }
                }
                _ if self.stack.is_empty() => {}
                Token::Binary => {}
                _ if self.skip > 0 => {
                    // One character standing in for a `\u`: a control word
                    // or symbol counts as one, and so does each byte of
                    // text.
                    match token {
                        Token::Text(bytes) if bytes.len() as u32 > self.skip => {
                            let rest = &bytes[self.skip as usize..];
                            self.skip = 0;
                            self.bytes(rest);
                        }
                        Token::Text(bytes) => self.skip -= bytes.len() as u32,
                        _ => self.skip -= 1,
                    }
                }
                Token::Word(word, param) => self.word(word, param),
                Token::Symbol(c) => self.symbol(c),
                Token::Hex(b) => self.bytes(&[b]),
                Token::Text(bytes) => self.bytes(bytes),
            }
        }
        Err(Error::Truncated)
    }

    fn close_group(&mut self) {
        let Some(closing) = self.stack.pop() else { return };
        let outer = self.stack.last().map(|s| s.dest);
        match closing.dest {
            // A font table entry's group, or the table's.
            Dest::FontTable => self.end_font(),
            Dest::Graphic if outer != Some(Dest::Graphic) => self.end_graphic(),
            Dest::InfoField(field) if outer != Some(closing.dest) => {
                let text = std::mem::take(&mut self.info).trim().to_owned();
                self.set_info(field, text);
            }
            _ => {}
        }
        if self.stack.is_empty() {
            // The document ends in the formatting of its outermost group.
            self.flush_high();
            self.last_para = Some(closing.para);
        }
    }

    /// An attachment's graphic read: its character and attachment, if the
    /// package has its file.
    fn end_graphic(&mut self) {
        let (name, width, height) = std::mem::take(&mut self.graphic);
        let name = String::from_utf8_lossy(&name).trim().to_owned();
        let Some((_, contents)) = self.files.iter().find(|(n, _)| *n == name) else { return };
        if self.stack.is_empty() || self.state().dest != Dest::Text {
            return;
        }
        let at = self.out.len();
        self.push_str("\u{FFFC}");
        if self.out.len() > at {
            self.attachments.push(super::model::Attachment {
                at,
                name,
                contents: contents.clone(),
                width: width.map_or(0.0, twips),
                height: height.map_or(0.0, twips),
            });
        }
    }

    fn set_info(&mut self, field: InfoField, text: String) {
        let a = &mut self.attrs;
        match field {
            InfoField::Title => a.title = Some(text),
            InfoField::Author => a.author = Some(text),
            InfoField::Subject => a.subject = Some(text),
            InfoField::Keywords => {
                a.keywords = text.split(',').map(|k| k.trim().to_owned()).filter(|k| !k.is_empty()).collect();
            }
            InfoField::Comment => a.comment = Some(text),
            InfoField::Company => a.company = Some(text),
            InfoField::Copyright => a.copyright = Some(text),
            InfoField::Editor => a.editor = Some(text),
            InfoField::Manager => a.manager = Some(text),
            InfoField::Category => a.category = Some(text),
        }
    }

    // Destinations.

    /// Handle `word` if it starts a destination; false if it doesn't.
    fn destination(&mut self, word: &[u8]) -> bool {
        let dest = self.state().dest;
        let info = |f| (dest == Dest::Info || matches!(dest, Dest::InfoField(_))).then_some(Dest::InfoField(f));
        let new = match word {
            b"fonttbl" => Some(Dest::FontTable),
            b"colortbl" => Some(Dest::ColorTable),
            b"expandedcolortbl" => Some(Dest::ExpandedColorTable),
            b"info" => Some(Dest::Info),
            b"title" => info(InfoField::Title),
            b"author" => info(InfoField::Author),
            b"subject" => info(InfoField::Subject),
            b"keywords" => info(InfoField::Keywords),
            b"doccomm" => info(InfoField::Comment),
            b"company" => info(InfoField::Company),
            b"copyright" => info(InfoField::Copyright),
            b"operator" => info(InfoField::Editor),
            b"manager" => info(InfoField::Manager),
            b"category" => info(InfoField::Category),
            b"field" => {
                self.fields.push(String::new());
                let index = self.fields.len() - 1;
                self.state().field = Some(index);
                return true;
            }
            b"fldinst" => {
                let index = match self.state().field {
                    Some(i) => i,
                    None => {
                        self.fields.push(String::new());
                        self.fields.len() - 1
                    }
                };
                Some(Dest::FieldInstruction(index))
            }
            b"fldrslt" => {
                let s = self.state();
                s.dest = Dest::Text;
                s.chr.link = s.field.or(s.chr.link);
                return true;
            }
            b"NeXTGraphic" => {
                // An attachment (RTFD's): its placeholder character, which
                // follows the group, goes with it.
                let n = self.stack.len();
                if n >= 2 {
                    self.stack[n - 2].after_attachment = true;
                }
                self.graphic = (Vec::new(), None, None);
                Some(Dest::Graphic)
            }
            // The text of a list item's marker, for readers without lists.
            b"listtext" | b"pntext" => Some(Dest::Text),
            b"stylesheet"
            | b"pict"
            | b"object"
            | b"header"
            | b"headerl"
            | b"headerr"
            | b"headerf"
            | b"footer"
            | b"footerl"
            | b"footerr"
            | b"footerf"
            | b"footnote"
            | b"annotation"
            | b"xmlnstbl"
            | b"listtable"
            | b"listoverridetable"
            | b"revtbl"
            | b"rsidtbl"
            | b"generator"
            | b"themedata"
            | b"colorschememapping"
            | b"datastore"
            | b"latentstyles"
            | b"pgdsctbl"
            | b"background"
            | b"shp"
            | b"shppict"
            | b"nonshppict"
            | b"bkmkstart"
            | b"bkmkend"
            | b"pn"
            | b"creatim"
            | b"revtim"
            | b"printim"
            | b"buptim"
            | b"userprops"
            | b"docvar"
            | b"template"
            | b"ftnsep"
            | b"ftnsepc"
            | b"ftncn"
            | b"aftnsep"
            | b"aftnsepc"
            | b"aftncn"
            | b"atnid"
            | b"atnauthor"
            | b"atndate"
            | b"atnref"
            | b"xe"
            | b"tc"
            | b"txe"
            | b"rxe"
            | b"mmathPr"
            | b"fldtype"
            | b"objdata"
            | b"blipuid"
            | b"panose"
            | b"falt"
            | b"fontemb"
            | b"fontfile" => Some(Dest::Skip),
            _ => return false,
        };
        match new {
            Some(d) => self.state().dest = d,
            None => return false,
        }
        true
    }

    // Control words.

    fn word(&mut self, word: &[u8], param: Option<i32>) {
        let ignorable = std::mem::take(&mut self.state().ignorable);
        let dest = self.state().dest;
        if dest == Dest::Skip {
            return;
        }
        if dest == Dest::Graphic {
            match word {
                b"width" => self.graphic.1 = param,
                b"height" => self.graphic.2 = param,
                _ => {}
            }
            return;
        }
        if self.destination(word) {
            return;
        }
        if ignorable {
            // An unknown destination: skipped whole.
            self.state().dest = Dest::Skip;
            return;
        }
        match dest {
            Dest::FontTable => return self.font_word(word, param),
            Dest::ColorTable | Dest::ExpandedColorTable => return self.color_word(word, param),
            _ => {}
        }
        if self.document_word(word, param) || self.paragraph_word(word, param) || self.character_word(word, param) {
            return;
        }
        let c = match word {
            b"par" => return self.end_paragraph(),
            b"line" => '\u{2028}',
            b"tab" => '\t',
            b"emdash" => '—',
            b"endash" => '–',
            b"emspace" => '\u{2003}',
            b"enspace" => '\u{2002}',
            b"qmspace" => '\u{2005}',
            b"bullet" => '•',
            b"lquote" => '‘',
            b"rquote" => '’',
            b"ldblquote" => '“',
            b"rdblquote" => '”',
            b"zwj" => '\u{200D}',
            b"zwnj" => '\u{200C}',
            b"zwbo" => '\u{200B}',
            b"zwnbo" => '\u{FEFF}',
            b"ltrmark" => '\u{200E}',
            b"rtlmark" => '\u{200F}',
            b"page" => '\u{0C}',
            b"cell" | b"nestcell" => '\t',
            b"row" | b"nestrow" => {
                // A row's last cell ends it, not a tab.
                if dest == Dest::Text {
                    self.flush_high();
                    self.out.pop_if('\t');
                    self.end_paragraph();
                }
                return;
            }
            b"u" => {
                let unit = param.unwrap_or(0);
                let unit = if unit < 0 { unit + 0x10000 } else { unit } as u32;
                self.unicode(unit);
                self.skip = self.state().uc;
                return;
            }
            _ => return,
        };
        self.push_char(c);
    }

    fn document_word(&mut self, word: &[u8], param: Option<i32>) -> bool {
        let n = param.unwrap_or(0);
        let a = &mut self.attrs;
        match word {
            b"ansi" => self.code_page = CodePage::Cp1252,
            b"mac" => self.code_page = CodePage::MacRoman,
            b"pc" => self.code_page = CodePage::Cp437,
            b"pca" => self.code_page = CodePage::Cp850,
            b"ansicpg" => {
                if let Some(page) = CodePage::from_number(n) {
                    self.code_page = page;
                }
            }
            // The default font, which AppKit doesn't use (text before any
            // `\f` is Helvetica, `\plain` is `\f0`): nor does Sidestep.
            b"deff" => {}
            b"deftab" => a.default_tab_interval = Some(twips(n)),
            b"paperw" => a.paper_size = Some((twips(n), a.paper_size.unwrap_or(super::model::PAPER).1)),
            b"paperh" => a.paper_size = Some((a.paper_size.unwrap_or(super::model::PAPER).0, twips(n))),
            b"margl" => a.left_margin = Some(twips(n)),
            b"margr" => a.right_margin = Some(twips(n)),
            b"margt" => a.top_margin = Some(twips(n)),
            b"margb" => a.bottom_margin = Some(twips(n)),
            b"vieww" => a.view_size = Some((twips(n), a.view_size.map_or(0.0, |v| v.1))),
            b"viewh" => a.view_size = Some((a.view_size.map_or(0.0, |v| v.0), twips(n))),
            b"viewscale" => a.view_zoom = Some(f64::from(n)),
            b"viewkind" => a.view_mode = Some(i64::from(n)),
            b"readonlydoc" => a.read_only = Some(i64::from(n)),
            b"hyphauto" if flag(param) => a.hyphenation_factor = Some(a.hyphenation_factor.unwrap_or(0.0)),
            b"hyphfactor" => a.hyphenation_factor = Some(f64::from(n) / 100.0),
            b"cocoartf" => {
                self.cocoa = true;
                a.cocoa_version = Some(f64::from(n));
            }
            b"cocoatextscaling" => a.text_scaling = Some(i64::from(n)),
            b"uc" => self.state().uc = n.max(0) as u32,
            _ => return false,
        }
        true
    }

    fn paragraph_word(&mut self, word: &[u8], param: Option<i32>) -> bool {
        let n = param.unwrap_or(0);
        let p = &mut self.state().para;
        match word {
            b"pard" => *p = Para::new(),
            b"ql" => p.alignment = Align::Left,
            b"qr" => p.alignment = Align::Right,
            b"qc" => p.alignment = Align::Center,
            b"qj" | b"qd" => p.alignment = Align::Justified,
            b"qn" => p.alignment = Align::Natural,
            b"li" => p.left = n,
            b"fi" => p.first = n,
            b"ri" => p.right = n,
            b"sb" => p.before = n,
            b"sa" => p.after = n,
            b"sl" => p.line = n,
            b"slmult" => p.line_multiple = flag(param),
            b"slminimum" => p.minimum = Some(n),
            b"slmaximum" => p.maximum = Some(n),
            b"slleading" => p.leading = n,
            b"pardeftab" => p.default_tab = Some(n),
            b"tqr" => p.next_tab = TabKind::Right,
            b"tqc" => p.next_tab = TabKind::Center,
            b"tqdec" => p.next_tab = TabKind::Decimal,
            b"tx" | b"tb" => {
                let kind = std::mem::replace(&mut p.next_tab, TabKind::Left);
                p.tabs.push(Tab { location: twips(n), kind });
            }
            b"pardirnatural" => p.direction = Direction::Natural,
            b"rtlpar" => p.direction = Direction::RightToLeft,
            b"ltrpar" => p.direction = Direction::LeftToRight,
            b"partightenfactor" => p.tightening = Some(n > 0),
            _ => return false,
        }
        p.set = true;
        true
    }

    fn character_word(&mut self, word: &[u8], param: Option<i32>) -> bool {
        let n = param.unwrap_or(0);
        let cocoa = self.cocoa;
        let c = &mut self.state().chr;
        let color = |n: i32| (n > 0).then_some(n as usize);
        match word {
            b"plain" => *c = Chr::plain(Some(0), c.link),
            b"f" => c.font = param,
            b"fs" if n > 0 => c.size = f64::from(n) / 2.0,
            b"fsmilli" if n > 0 => c.size = f64::from(n) / 1000.0,
            b"b" => c.bold = flag(param),
            b"i" => c.italic = flag(param),
            b"ul" => c.underline = if flag(param) { 1 } else { 0 },
            b"ulnone" => c.underline = 0,
            b"uld" => c.underline = 1 | 0x100,
            b"uldash" | b"ulldash" => c.underline = 1 | 0x200,
            b"uldashd" => c.underline = 1 | 0x300,
            b"uldashdd" => c.underline = 1 | 0x400,
            b"uldb" => c.underline = 9,
            b"ulth" => c.underline = 2,
            b"ulthd" => c.underline = 2 | 0x100,
            b"ulthdash" | b"ulthldash" => c.underline = 2 | 0x200,
            b"ulthdashd" => c.underline = 2 | 0x300,
            b"ulthdashdd" => c.underline = 2 | 0x400,
            b"ulw" => c.underline = 1 | 0x8000,
            b"ulwave" | b"ulhwave" | b"ululdbwave" => c.underline = 1,
            b"ulc" => c.underline_color = color(n),
            b"strike" => c.strikethrough = if flag(param) { 1 } else { 0 },
            b"striked" => c.strikethrough = if flag(param) { 9 } else { 0 },
            b"strikestyle" if n > 0 => c.strikethrough = i64::from(n),
            b"strikec" => c.strikethrough_color = color(n),
            b"cf" => c.foreground = n.max(0) as usize,
            b"cb" => c.background = if cocoa && n == 1 { None } else { color(n) },
            b"highlight" | b"chcbpat" => c.background = color(n),
            b"up" => c.baseline_offset = f64::from(n) / 2.0,
            b"dn" => c.baseline_offset = -f64::from(n) / 2.0,
            b"super" => c.superscript = i64::from(param.unwrap_or(1).max(1)),
            b"sub" => c.superscript = -i64::from(param.unwrap_or(1).max(1)),
            b"nosupersub" => c.superscript = 0,
            b"expnd" => c.kern = (n != 0).then(|| f64::from(n) / 4.0),
            b"expndtw" => c.kern = (n != 0).then(|| twips(n)),
            b"shad" => c.shadow = flag(param),
            b"shadx" => c.shadow_x = twips(n),
            b"shady" => c.shadow_y = twips(n),
            b"shadr" => c.shadow_blur = twips(n),
            b"shado" => c.shadow_opacity = f64::from(n.clamp(0, 255)) / 255.0,
            b"shadc" => c.shadow_color = n.max(0) as usize,
            b"expansion" => c.expansion = f64::from(n) / 2000.0,
            b"obliqueness" => c.obliqueness = f64::from(n) / 2000.0,
            b"outl" => c.outline = flag(param),
            b"strokewidth" => c.stroke_width = Some(twips(n)),
            b"strokec" => c.stroke_color = color(n),
            b"CocoaLigature" => c.ligature = Some(i64::from(n)),
            _ => return false,
        }
        true
    }

    fn font_word(&mut self, word: &[u8], param: Option<i32>) {
        match word {
            b"f" => {
                self.end_font();
                self.font = Some(FontEntry {
                    number: param.unwrap_or(0),
                    name: String::new(),
                    generic: Generic::Sans,
                    code_page: None,
                });
            }
            b"froman" => self.set_font_class(Generic::Serif),
            b"fmodern" => self.set_font_class(Generic::Mono),
            b"fcharset" => {
                if let Some(f) = &mut self.font {
                    f.code_page = CodePage::from_charset(param.unwrap_or(0));
                }
            }
            b"u" => {
                let unit = param.unwrap_or(0);
                if let Some(c) = char::from_u32(if unit < 0 { unit + 0x10000 } else { unit } as u32) {
                    self.font_name.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
                self.skip = self.state().uc;
            }
            _ => {}
        }
    }

    fn set_font_class(&mut self, generic: Generic) {
        if let Some(f) = &mut self.font {
            f.generic = generic;
        }
    }

    /// Finish the font table entry being read, if any.
    fn end_font(&mut self) {
        let Some(mut entry) = self.font.take() else { return };
        let bytes = std::mem::take(&mut self.font_name);
        // Names are ASCII or UTF-8 (from `\u`) as a rule; other bytes are
        // in the entry's code page.
        let name = match std::str::from_utf8(&bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => bytes.iter().map(|&b| entry.code_page.unwrap_or(self.code_page).decode(b)).collect(),
        };
        entry.name = name.trim().trim_end_matches(';').trim().to_owned();
        self.fonts.retain(|f| f.number != entry.number);
        self.fonts.push(entry);
    }

    fn color_word(&mut self, word: &[u8], param: Option<i32>) {
        let n = param.unwrap_or(0);
        let e = &mut self.color_entry;
        match word {
            b"red" => e.rgb[0] = Some(n),
            b"green" => e.rgb[1] = Some(n),
            b"blue" => e.rgb[2] = Some(n),
            b"cssrgb" => e.space = Some(Space::Srgb),
            b"csgenericrgb" => e.space = Some(Space::GenericRgb),
            b"cspthree" => e.space = Some(Space::DisplayP3),
            b"csgray" => e.space = Some(Space::Gray),
            b"cscmyk" => e.space = Some(Space::Cmyk),
            b"c" => e.components.push(n),
            b"cname" => e.naming = true,
            _ => {}
        }
    }

    // Symbols and text.

    fn symbol(&mut self, c: u8) {
        if self.state().dest == Dest::Skip {
            return;
        }
        match c {
            b'*' => self.state().ignorable = true,
            b'\\' | b'{' | b'}' => self.bytes(&[c]),
            b'~' => self.push_char('\u{A0}'),
            b'_' => self.push_char('\u{2011}'),
            b'\n' | b'\r' => self.end_paragraph(),
            b'\t' => self.push_char('\t'),
            // `\-` (an optional hyphen), `\:` (an index subentry) and
            // `\|` (a formula character) show nothing.
            _ => {}
        }
    }

    /// Bytes of text in the current destination.
    fn bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let dest = self.state().dest;
        match dest {
            Dest::Skip | Dest::Info => {}
            Dest::Graphic => self.graphic.0.extend_from_slice(bytes),
            Dest::FontTable => {
                if self.font.is_none() {
                    // A name without `\f`: the first entry, numbered 0.
                    self.font =
                        Some(FontEntry { number: 0, name: String::new(), generic: Generic::Sans, code_page: None });
                }
                for &b in bytes {
                    if b == b';' {
                        self.end_font();
                    } else if self.font.is_some() {
                        self.font_name.push(b);
                    }
                }
            }
            Dest::ColorTable | Dest::ExpandedColorTable => {
                for &b in bytes {
                    if b == b';' {
                        let entry = std::mem::take(&mut self.color_entry);
                        if dest == Dest::ColorTable {
                            self.colors.push(entry.plain());
                        } else {
                            self.expanded.push(entry.expanded());
                        }
                    } else if self.color_entry.naming && !b.is_ascii_whitespace() || !self.color_entry.name.is_empty() {
                        self.color_entry.name.push(char::from(b));
                    }
                }
            }
            _ => {
                let page = self.text_code_page();
                let mut bytes = bytes;
                if std::mem::take(&mut self.state().after_attachment) && bytes[0] == 0xAC {
                    bytes = &bytes[1..];
                }
                let text: String = bytes.iter().map(|&b| page.decode(b)).collect();
                self.push_str(&text);
            }
        }
    }

    /// The code page text in the current font is in.
    fn text_code_page(&mut self) -> CodePage {
        let font = self.state().chr.font;
        font.and_then(|n| self.fonts.iter().find(|f| f.number == n)).and_then(|f| f.code_page).unwrap_or(self.code_page)
    }

    /// A UTF-16 unit from `\u`.
    fn unicode(&mut self, unit: u32) {
        let unit = unit as u16;
        if (0xD800..0xDC00).contains(&unit) {
            self.flush_high();
            self.high = Some(unit);
            return;
        }
        if (0xDC00..0xE000).contains(&unit) {
            if let Some(high) = self.high.take() {
                let c = 0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                self.push_char(char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER));
            } else {
                self.push_char(char::REPLACEMENT_CHARACTER);
            }
            return;
        }
        self.push_char(char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER));
    }

    /// A high surrogate with no low half after it stands for nothing
    /// valid.
    fn flush_high(&mut self) {
        if self.high.take().is_some() {
            self.push_raw_char(char::REPLACEMENT_CHARACTER);
        }
    }

    fn push_char(&mut self, c: char) {
        self.flush_high();
        self.push_raw_char(c);
    }

    fn push_raw_char(&mut self, c: char) {
        self.push_str(c.encode_utf8(&mut [0; 4]));
    }

    /// Text in the current destination.
    fn push_str(&mut self, text: &str) {
        if self.high.is_some() {
            self.flush_high();
        }
        if self.stack.is_empty() {
            return;
        }
        match self.state().dest {
            Dest::Text => {
                let style = self.char_style();
                self.out.push(text, &style);
            }
            Dest::InfoField(_) => self.info.push_str(text),
            Dest::FieldInstruction(i) => self.fields[i].push_str(text),
            Dest::FontTable => self.font_name.extend_from_slice(text.as_bytes()),
            _ => {}
        }
    }

    fn end_paragraph(&mut self) {
        if self.state().dest != Dest::Text {
            return;
        }
        self.flush_high();
        let style = self.char_style();
        let para = self.state().para.clone();
        let para = self.para_style(&para);
        self.out.end_paragraph("\n", &style, para.as_ref());
    }

    // Resolving formatting.

    fn char_style(&mut self) -> CharStyle {
        let chr = self.state().chr.clone();
        if let Some((c, style)) = &self.resolved
            && *c == chr
        {
            return style.clone();
        }
        let style = self.resolve(&chr);
        self.resolved = Some((chr, style.clone()));
        style
    }

    fn color(&self, index: usize) -> Option<Color> {
        if index == 0 {
            return None;
        }
        match self.expanded.get(index) {
            Some(Some(c)) => Some(c.clone()),
            _ => self.colors.get(index).cloned().flatten(),
        }
    }

    fn resolve(&self, chr: &Chr) -> CharStyle {
        let entry = chr.font.and_then(|n| self.fonts.iter().find(|f| f.number == n));
        let (name, generic) = entry.map_or(("Helvetica", Generic::Sans), |e| (e.name.as_str(), e.generic));
        let font = Font {
            bold: chr.bold,
            italic: chr.italic,
            ..Font::named(if name.is_empty() { "Helvetica" } else { name }, generic, chr.size)
        };
        let shadow = chr.shadow.then(|| {
            let color = self.color(chr.shadow_color).unwrap_or_else(|| Color::in_space(Space::Gray, vec![0.0], 1.0));
            let mut components = color.components.clone();
            if let Some(a) = components.last_mut() {
                *a *= chr.shadow_opacity;
            }
            let mut srgb = color.srgb;
            srgb[3] *= chr.shadow_opacity;
            Shadow {
                offset: (chr.shadow_x, chr.shadow_y),
                blur: chr.shadow_blur,
                color: Some(Color { components, srgb, ..color }),
            }
        });
        let stroke_width = chr.stroke_width.unwrap_or(if chr.outline { 3.0 } else { 0.0 });
        CharStyle {
            font: Some(font),
            color: self.color(chr.foreground),
            background: chr.background.and_then(|i| self.color(i)),
            underline: chr.underline,
            underline_color: chr.underline_color.and_then(|i| self.color(i)).filter(|_| chr.underline != 0),
            strikethrough: chr.strikethrough,
            strikethrough_color: chr.strikethrough_color.and_then(|i| self.color(i)).filter(|_| chr.strikethrough != 0),
            link: chr.link.and_then(|i| hyperlink(&self.fields[i])),
            baseline_offset: chr.baseline_offset,
            superscript: chr.superscript,
            kern: chr.kern,
            ligature: chr.ligature,
            shadow,
            expansion: chr.expansion,
            obliqueness: chr.obliqueness,
            stroke_width,
            stroke_color: chr.stroke_color.and_then(|i| self.color(i)).filter(|_| stroke_width != 0.0),
        }
    }

    fn para_style(&self, p: &Para) -> Option<ParaStyle> {
        if !p.set {
            return None;
        }
        // In floating point, so no parameter overflows.
        let (mut minimum, mut maximum, mut multiple) =
            (p.minimum.map_or(0.0, twips), p.maximum.map_or(0.0, twips), 0.0);
        if p.line_multiple && p.line > 0 {
            multiple = f64::from(p.line) / 240.0;
        } else if p.line > 0 && p.minimum.is_none() {
            minimum = twips(p.line);
        } else if p.line < 0 {
            (minimum, maximum) = (-twips(p.line), -twips(p.line));
        }
        let tail = if p.right == 0 { 0.0 } else { self.attrs.text_width() - twips(p.right) };
        Some(ParaStyle {
            alignment: p.alignment,
            first_line_head_indent: (f64::from(p.left) + f64::from(p.first)) / 20.0,
            head_indent: twips(p.left),
            tail_indent: tail,
            line_spacing: twips(p.leading),
            paragraph_spacing: twips(p.after),
            paragraph_spacing_before: twips(p.before),
            minimum_line_height: minimum,
            maximum_line_height: maximum,
            line_height_multiple: multiple,
            direction: p.direction,
            tabs: p.tabs.clone(),
            default_tab_interval: p.default_tab.map(twips).unwrap_or_else(|| self.default_tab_interval()),
            header_level: 0,
            tightening: p.tightening.unwrap_or(true),
        })
    }

    /// The document's default tab interval: `\deftab`'s, else RTF's
    /// default of half an inch, or none in Cocoa's RTF (as AppKit reads
    /// it).
    fn default_tab_interval(&self) -> f64 {
        self.attrs.default_tab_interval.unwrap_or(if self.cocoa { 0.0 } else { 36.0 })
    }

    fn finish(mut self) -> Doc {
        let last = self.last_para.take().and_then(|p| self.para_style(&p));
        let default_tab = self.default_tab_interval();
        let mut attrs = std::mem::take(&mut self.attrs);
        attrs.default_tab_interval = Some(default_tab);
        let mut doc = self.out.finish(last.as_ref());
        doc.attrs = attrs;
        doc.attachments = self.attachments;
        doc
    }
}

/// The target of a hyperlink field's instruction: its first argument,
/// quoted or not, or `#anchor` for `\l "anchor"`. None for other fields.
pub(crate) fn hyperlink(instruction: &str) -> Option<String> {
    let rest = instruction.trim_start();
    let rest = rest.strip_prefix("HYPERLINK").or_else(|| rest.strip_prefix("hyperlink"))?;
    let mut args = Vec::new();
    let mut chars = rest.chars().peekable();
    let mut anchor = false;
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '\\' {
            chars.next();
            let switch: String = std::iter::from_fn(|| chars.next_if(|c| c.is_ascii_alphabetic())).collect();
            anchor |= switch == "l";
            // A switch's argument is taken with it (`\o "tip"`), but for
            // `\l`, whose argument is the anchor.
            if switch != "l" {
                while chars.next_if(|c| c.is_whitespace()).is_some() {}
                if chars.peek() == Some(&'"') {
                    chars.next();
                    while chars.next().is_some_and(|c| c != '"') {}
                }
            }
            continue;
        }
        let arg: String = if c == '"' {
            chars.next();
            let mut s = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' if chars.peek() == Some(&'"') => s.push(chars.next().unwrap_or('"')),
                    _ => s.push(c),
                }
            }
            s
        } else {
            std::iter::from_fn(|| chars.next_if(|c| !c.is_whitespace())).collect()
        };
        args.push((arg, anchor));
        anchor = false;
    }
    let mut target: Option<String> = None;
    for (arg, anchor) in args {
        match (anchor, &mut target) {
            (true, Some(t)) => {
                t.push('#');
                t.push_str(&arg);
            }
            (true, None) => target = Some(format!("#{arg}")),
            (false, None) => target = Some(arg),
            (false, Some(_)) => {}
        }
    }
    target.filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::super::model::default_tabs;
    use super::*;

    fn doc(rtf: &str) -> Doc {
        read(rtf.as_bytes()).expect("RTF")
    }

    fn runs(d: &Doc) -> Vec<(&str, &CharStyle)> {
        d.run_ranges().map(|(r, s)| (&d.text[r], s)).collect()
    }

    const COCOA: &str = "{\\rtf1\\ansi\\ansicpg1252\\cocoartf2870\n\\cocoatextscaling0\\cocoaplatform0{\\fonttbl\\f0\\fswiss\\fcharset0 Helvetica;\\f1\\fswiss\\fcharset0 Helvetica-Bold;}\n{\\colortbl;\\red255\\green255\\blue255;\\red251\\green0\\blue7;}\n{\\*\\expandedcolortbl;;\\cssrgb\\c100000\\c0\\c0;}\n\\pard\\tx560\\tx1120\\tx1680\\tx2240\\tx2800\\tx3360\\tx3920\\tx4480\\tx5040\\tx5600\\tx6160\\tx6720\\pardirnatural\\partightenfactor0\n\n\\f0\\fs24 \\cf0 Normal \n\\f1\\b Bold\n\\f0\\b0  \\cf2 red\\cf0 \\\nnext}";

    #[test]
    fn cocoa_rtf_reads_as_appkit_writes_it() {
        let d = doc(COCOA);
        assert_eq!(d.text, "Normal Bold red\nnext");
        let r = runs(&d);
        assert_eq!(r[0].0, "Normal ");
        let font = r[0].1.font.as_ref().unwrap();
        assert_eq!((font.names[0].as_str(), font.size, font.bold), ("Helvetica", 12.0, false));
        assert_eq!(r[1].0, "Bold");
        let bold = r[1].1.font.as_ref().unwrap();
        assert_eq!((bold.names[0].as_str(), bold.bold), ("Helvetica-Bold", true));
        assert_eq!(r[3].0, "red");
        let red = r[3].1.color.as_ref().unwrap();
        assert_eq!((red.space, red.components.as_slice()), (Space::Srgb, &[1.0, 0.0, 0.0, 1.0][..]));
        assert!(r.iter().filter(|(t, _)| *t != "red").all(|(_, s)| s.color.is_none()));
        assert_eq!(d.paras.len(), 2);
        let para = d.paras[0].1.as_ref().unwrap();
        assert_eq!((para.alignment, para.direction, &para.tabs), (Align::Left, Direction::Natural, &default_tabs()));
        assert_eq!((para.default_tab_interval, para.tightening), (0.0, false));
        assert_eq!(d.attrs.cocoa_version, Some(2870.0));
    }

    #[test]
    fn groups_keep_formatting_and_plain_resets_it() {
        let d = doc(
            "{\\rtf1{\\fonttbl\\f0\\fnil Helvetica;}\\f0 {\\b bold {\\i both} bold} plain\\b1 on\\b0 off\\fs40\\ul\\plain  p}",
        );
        let r = runs(&d);
        let traits: Vec<(&str, bool, bool)> =
            r.iter().map(|(t, s)| (*t, s.font.as_ref().unwrap().bold, s.font.as_ref().unwrap().italic)).collect();
        assert_eq!(
            traits,
            [
                ("bold ", true, false),
                ("both", true, true),
                (" bold", true, false),
                (" plain", false, false),
                ("on", true, false),
                ("off p", false, false)
            ]
        );
        assert!(r.last().unwrap().1.underline == 0 && r.last().unwrap().1.font.as_ref().unwrap().size == 12.0);
        assert!(d.paras[0].1.is_none(), "no paragraph formatting, no paragraph style");
    }

    #[test]
    fn characters_code_pages_and_unicode() {
        let d = doc(
            "{\\rtf1\\ansi{\\fonttbl\\f0\\fnil Helvetica;}\\f0 caf\\'e9 \\'80 \\uc2\\u233\\'65\\'65x \\uc0\\u26085 {\\uc1\\u233 ?z}\\u-10179\\u-8704 q\\~\\_\\-\\emdash\\line\\tab\\\\\\{\\}}",
        );
        assert_eq!(d.text, "café € éx 日éz😀q\u{A0}\u{2011}—\u{2028}\t\\{}");
        let d = doc(
            "{\\rtf1\\ansi\\ansicpg1251{\\fonttbl\\f0\\fnil Helvetica;\\f1\\fnil\\fcharset238 Helvetica;}\\f0 \\'c0\\f1 \\'8a}",
        );
        assert_eq!(d.text, "АŠ");
        assert_eq!(doc("{\\rtf1\\mac \\'8e}").text, "é");
        // A lone high surrogate reads as a replacement character.
        assert_eq!(doc("{\\rtf1\\uc0\\u55357 x}").text, "\u{FFFD}x");
    }

    #[test]
    fn paragraphs_take_the_formatting_at_their_end() {
        let d = doc(
            "{\\rtf1{\\fonttbl\\f0\\fnil Helvetica;}\\f0\\pard\\qc a\\qr b\\par c\\par\\pard\\li720\\fi-360\\ri360\\sb100\\sa200\\sl-300 d\\par\\pard\\sl360\\slmult1\\tx1000\\tqr\\tx3000\\tqdec\\tx4000 e}",
        );
        assert_eq!(d.text, "ab\nc\nd\ne");
        let styles: Vec<&ParaStyle> = d.paras.iter().map(|p| p.1.as_ref().unwrap()).collect();
        assert_eq!((styles[0].alignment, styles[1].alignment), (Align::Right, Align::Right));
        let s = styles[2];
        assert_eq!((s.head_indent, s.first_line_head_indent, s.tail_indent), (36.0, 18.0, 432.0 - 18.0));
        assert_eq!((s.paragraph_spacing_before, s.paragraph_spacing), (5.0, 10.0));
        assert_eq!((s.minimum_line_height, s.maximum_line_height), (15.0, 15.0));
        assert_eq!((s.direction, s.default_tab_interval, s.tabs.len()), (Direction::LeftToRight, 36.0, 0));
        let e = styles[3];
        assert_eq!(e.line_height_multiple, 1.5);
        let kinds: Vec<_> = e.tabs.iter().map(|t| (t.location, t.kind)).collect();
        assert_eq!(kinds, [(50.0, TabKind::Left), (150.0, TabKind::Right), (200.0, TabKind::Decimal)]);
    }

    #[test]
    fn fields_become_links() {
        let d = doc(
            "{\\rtf1 See {\\field{\\*\\fldinst{HYPERLINK \"https://example.com/a?b=c&d\"}}{\\fldrslt {\\b example}}} {\\field{\\*\\fldinst HYPERLINK \\\\l \"top\"}{\\fldrslt up}} {\\field{\\*\\fldinst PAGE}{\\fldrslt 7}}.}",
        );
        assert_eq!(d.text, "See example up 7.");
        let r = runs(&d);
        let link = |t: &str| r.iter().find(|(x, _)| *x == t).and_then(|(_, s)| s.link.clone());
        assert_eq!(link("example").as_deref(), Some("https://example.com/a?b=c&d"));
        assert_eq!(link("up").as_deref(), Some("#top"));
        assert_eq!(link(" 7."), None);
        assert_eq!(hyperlink("HYPERLINK \"http://a.b/c\" \\o \"tip\""), Some("http://a.b/c".into()));
        assert_eq!(hyperlink(" HYPERLINK http://x.y/"), Some("http://x.y/".into()));
        assert_eq!(hyperlink("PAGE"), None);
    }

    #[test]
    fn destinations_are_skipped() {
        let d = doc(
            "{\\rtf1{\\fonttbl{\\f0\\froman\\fcharset0 Times New Roman{\\*\\panose 02020603050405020304};}}{\\stylesheet{\\s0 Normal;}}{\\*\\generator LibreOffice}{\\info{\\title Tt}{\\author A}{\\creatim\\yr2020}{\\keywords k1, k2}}\\f0 {\\*\\unknown skip me} body {\\pict\\pngblip 8950} end\\bin3 abc}",
        );
        assert_eq!(d.text, " body  end");
        let font = runs(&d)[0].1.font.clone().unwrap();
        assert_eq!((font.names[0].as_str(), font.generic), ("Times New Roman", Generic::Serif));
        assert_eq!((d.attrs.title.as_deref(), d.attrs.author.as_deref()), (Some("Tt"), Some("A")));
        assert_eq!(d.attrs.keywords, ["k1", "k2"]);
        assert_eq!(d.attrs.default_tab_interval, Some(36.0), "not Cocoa's RTF: 36 points");
    }

    #[test]
    fn character_formatting() {
        let d = doc(
            "{\\rtf1\\cocoartf2870{\\fonttbl\\f0\\fnil Helvetica;}{\\colortbl;\\red255\\green255\\blue255;\\red0\\green0\\blue255;}\\f0\\ulw a\\ulnone \\uldb\\ulc2 b\\ul0 \\strike\\strikec2 c\\strike0 \\up6 d\\up0 \\super e\\nosupersub \\expndtw30 f\\expndtw0 \\shad\\shadx40\\shady-40\\shadr60\\shado85 \\shadc0 g\\shad0 \\outl0\\strokewidth-60 \\strokec2 h\\strokewidth0 \\expansion400 i\\expansion0 \\cb2 j\\cb1 k\\fs37 l\\fsmilli13300 m}",
        );
        let r = runs(&d);
        let get = |t: &str| r.iter().find(|(x, _)| *x == t).map(|(_, s)| (*s).clone()).unwrap();
        assert_eq!(get("a").underline, 0x8001);
        let b = get("b");
        assert_eq!((b.underline, b.underline_color.map(|c| c.srgb)), (9, Some([0.0, 0.0, 1.0, 1.0])));
        assert_eq!(get("c").strikethrough, 1);
        assert_eq!(get("d").baseline_offset, 3.0);
        assert_eq!(get("e").superscript, 1);
        assert_eq!(get("f").kern, Some(1.5));
        let g = get("g").shadow.unwrap();
        assert_eq!((g.offset, g.blur), ((2.0, -2.0), 3.0));
        assert!((g.color.unwrap().alpha() - 1.0 / 3.0).abs() < 0.01);
        let h = get("h");
        assert_eq!((h.stroke_width, h.stroke_color.is_some()), (-3.0, true));
        assert_eq!(get("i").expansion, 0.2);
        assert!(get("j").background.is_some() && get("k").background.is_none());
        assert_eq!(get("l").font.unwrap().size, 18.5);
        assert_eq!(get("m").font.unwrap().size, 13.3);
    }

    #[test]
    fn documents_and_errors() {
        let d = doc(
            "{\\rtf1\\cocoartf2870\\readonlydoc1{\\info{\\title T}}\\paperw10000\\paperh14000\\margl1000\\margr1200\\margb400\\margt200\\vieww6000\\viewh8000\\viewscale150\\viewkind1\\hyphauto1\\hyphfactor50\\deftab600 x}",
        );
        let a = &d.attrs;
        assert_eq!((a.paper_size, a.left_margin, a.right_margin), (Some((500.0, 700.0)), Some(50.0), Some(60.0)));
        assert_eq!((a.top_margin, a.bottom_margin, a.view_size), (Some(10.0), Some(20.0), Some((300.0, 400.0))));
        assert_eq!((a.view_zoom, a.view_mode, a.read_only), (Some(150.0), Some(1), Some(1)));
        assert_eq!((a.hyphenation_factor, a.default_tab_interval), (Some(0.5), Some(30.0)));
        assert_eq!(read(b"garbage"), Err(Error::NotRtf));
        assert_eq!(read(b"{\\rtf1 unterminated {\\b bold"), Err(Error::Truncated));
        assert_eq!(doc("  {\\rtf1 x}").text, "x");
        assert_eq!(doc("{\\rtf1 }").text, "");
        // Table cells and rows.
        assert_eq!(doc("{\\rtf1 \\trowd a\\cell b\\cell\\row c}").text, "a\tb\nc");
    }

    #[test]
    fn missing_fonts_and_colors_fall_back() {
        let d = doc(
            "{\\rtf1{\\fonttbl\\f0\\fswiss Helvetica;}{\\colortbl;\\red1\\green2\\blue3;}\\f0\\fs24 \\f99 a\\cf5 b\\cf1 c}",
        );
        let r = runs(&d);
        assert!(r.iter().all(|(_, s)| s.font.as_ref().unwrap().names[0] == "Helvetica"));
        assert!(r[0].1.color.is_none());
        let c = r.last().unwrap().1.color.as_ref().unwrap();
        assert_eq!(c.space, Space::GenericRgb);
        // No font table at all: Helvetica 12.
        let d = doc("{\\rtf1 plain}");
        let f = runs(&d)[0].1.font.clone().unwrap();
        assert_eq!((f.names[0].as_str(), f.size), ("Helvetica", 12.0));
        // As AppKit reads them: text before any `\f` is Helvetica, whatever
        // `\deff` says; `\plain` is `\f0`, a font the table lacks Helvetica.
        let name = |rtf: &str| -> Vec<String> {
            runs(&doc(rtf)).iter().map(|(_, s)| s.font.as_ref().unwrap().names[0].clone()).collect()
        };
        assert_eq!(name("{\\rtf1\\deff1{\\fonttbl{\\f0 Times;}{\\f1 Courier;}}x}"), ["Helvetica"]);
        assert_eq!(name("{\\rtf1\\deff1{\\fonttbl{\\f0 Times;}{\\f1 Courier;}}\\f1 a\\plain b}"), ["Courier", "Times"]);
        assert_eq!(name("{\\rtf1{\\fonttbl{\\f3 Times;}{\\f4 Courier;}}\\f9 a}"), ["Helvetica"]);
        // And the code page: the document's until a font gives its own.
        assert_eq!(doc("{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0\\fcharset204 Times;}}\\'c0\\f0\\'c0}").text, "ÀА");
    }

    /// Parameters at the ends of their range, and past them, read as
    /// AppKit reads them (in floating point, 32 bits kept) and never
    /// overflow.
    #[test]
    fn extreme_parameters() {
        let style = |rtf: &str| doc(rtf).paras[0].1.clone().unwrap();
        let s = style("{\\rtf1\\pard\\li2147483647\\fi1 x\\par}");
        assert_eq!(s.head_indent, 2147483647.0 / 20.0);
        assert_eq!(s.first_line_head_indent, 2147483648.0 / 20.0);
        let s = style("{\\rtf1\\pard\\li-2147483648\\fi-1 x\\par}");
        assert_eq!(s.first_line_head_indent, -2147483649.0 / 20.0);
        let s = style("{\\rtf1\\pard\\sl-2147483648 x\\par}");
        assert_eq!((s.minimum_line_height, s.maximum_line_height), (2147483648.0 / 20.0, 2147483648.0 / 20.0));
        // All the digits are the parameter's, their low 32 bits kept.
        let d = doc("{\\rtf1\\pard\\li99999999999 x\\par}");
        assert_eq!((d.text.as_str(), d.paras[0].1.as_ref().unwrap().head_indent), ("x\n", 1215752191.0 / 20.0));
        assert!(doc("{\\rtf1\\fs999999999999999999999999 x\\u99999999999999999999 y}").text.starts_with('x'));
    }
}
