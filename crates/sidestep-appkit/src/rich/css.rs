//! The CSS the HTML reader needs: declarations (a `style` attribute's, and
//! the rules of `<style>` sheets with simple selectors), and the values it
//! reads (colors, lengths, font families and shorthands).
//!
//! Selectors are type, class and universal selectors and their compounds
//! (`p`, `.s1`, `span.s1`, `*`), in lists; a selector with combinators,
//! attributes or pseudo-classes matches nothing, and at-rules are skipped.
//! Rules apply in order of specificity, then of appearance.

use super::tables::css_color;

/// A declaration: a property (lowercase) and its value.
pub(crate) type Declaration = (String, String);

/// The declarations of a `style` attribute or a rule's block.
pub(crate) fn declarations(text: &str) -> Vec<Declaration> {
    let text = strip_comments(text);
    let mut out = Vec::new();
    for part in split_top(&text, ';') {
        let Some((name, value)) = part.split_once(':') else { continue };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().trim_end_matches("!important").trim_end().to_owned();
        if !name.is_empty() && !value.is_empty() {
            out.push((name, value));
        }
    }
    out
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = rest[i + 2..].find("*/").map_or("", |j| &rest[i + 2 + j + 2..]);
    }
    out.push_str(rest);
    out
}

/// `text` split at `sep` outside parentheses and quotes.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut start) = (0i32, None, 0);
    for (i, c) in text.char_indices() {
        match c {
            '"' | '\'' if quote == Some(c) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '(' if quote.is_none() => depth += 1,
            ')' if quote.is_none() => depth -= 1,
            _ if c == sep && depth == 0 && quote.is_none() => {
                parts.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// A simple selector: a compound of an element type and classes.
#[derive(Clone, Debug, PartialEq)]
struct Selector {
    element: Option<String>,
    classes: Vec<String>,
}

impl Selector {
    fn parse(text: &str) -> Option<Selector> {
        let text = text.trim();
        if text.is_empty() || text.contains(|c: char| c.is_whitespace() || "[]:>+~#()".contains(c)) {
            return None;
        }
        let mut parts = text.split('.');
        let element = parts.next()?;
        let element = match element {
            "" | "*" => None,
            e => Some(e.to_ascii_lowercase()),
        };
        let classes: Vec<String> = parts.map(str::to_owned).collect();
        if classes.iter().any(String::is_empty) {
            return None;
        }
        Some(Selector { element, classes })
    }

    fn specificity(&self) -> (usize, usize) {
        (self.classes.len(), usize::from(self.element.is_some()))
    }

    fn matches(&self, element: &str, classes: &[&str]) -> bool {
        self.element.as_deref().is_none_or(|e| e == element)
            && self.classes.iter().all(|c| classes.contains(&c.as_str()))
    }
}

/// The rules of `<style>` sheets.
#[derive(Clone, Debug, Default)]
pub(crate) struct Sheet {
    rules: Vec<(Selector, Vec<Declaration>)>,
}

impl Sheet {
    /// Add the rules of a sheet's text.
    pub fn add(&mut self, text: &str) {
        let text = strip_comments(text);
        let mut rest = text.as_str();
        while let Some(open) = rest.find('{') {
            let prelude = rest[..open].trim();
            // The block: to its matching brace.
            let mut depth = 0;
            let mut close = rest.len();
            for (i, c) in rest[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = open + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let block = &rest[open + 1..close.min(rest.len())];
            // At-rules (`@media`, `@font-face`, …) are skipped; a statement
            // at-rule before this rule (`@import …;`) is dropped with it.
            let prelude = prelude.rsplit(';').next().unwrap_or("").trim();
            if !prelude.starts_with('@') && !prelude.is_empty() {
                let decls = declarations(block);
                for selector in prelude.split(',').filter_map(Selector::parse) {
                    self.rules.push((selector, decls.clone()));
                }
            }
            rest = rest.get(close + 1..).unwrap_or("");
        }
    }

    /// The declarations that apply to an element, by specificity, then
    /// order.
    pub fn matching(&self, element: &str, classes: &[&str]) -> Vec<Declaration> {
        let mut found: Vec<(&Selector, usize)> = self
            .rules
            .iter()
            .enumerate()
            .filter(|(_, (s, _))| s.matches(element, classes))
            .map(|(i, (s, _))| (s, i))
            .collect();
        found.sort_by_key(|(s, i)| (s.specificity(), *i));
        found.into_iter().flat_map(|(_, i)| self.rules[i].1.iter().cloned()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

// Values.

/// A color value: sRGB with alpha, or `None` for `transparent`. Unknown
/// values are `Err`.
pub(crate) fn color(value: &str) -> Result<Option<[f64; 4]>, ()> {
    let v = value.trim().to_ascii_lowercase();
    if v == "transparent" {
        return Ok(None);
    }
    if let Some(hex) = v.strip_prefix('#') {
        let digits: Vec<u32> = hex.chars().map(|c| c.to_digit(16)).collect::<Option<_>>().ok_or(())?;
        let (r, g, b, a) = match digits.len() {
            3 | 4 => {
                let d = |i: usize| f64::from(digits[i] * 17) / 255.0;
                (d(0), d(1), d(2), if digits.len() == 4 { d(3) } else { 1.0 })
            }
            6 | 8 => {
                let d = |i: usize| f64::from(digits[i] * 16 + digits[i + 1]) / 255.0;
                (d(0), d(2), d(4), if digits.len() == 8 { d(6) } else { 1.0 })
            }
            _ => return Err(()),
        };
        return Ok(Some([r, g, b, a]));
    }
    if let Some(args) = function(&v, &["rgb", "rgba"]) {
        let n = args.len();
        if n < 3 {
            return Err(());
        }
        let channel = |s: &str| match s.strip_suffix('%') {
            Some(p) => p.trim().parse::<f64>().map(|p| p / 100.0),
            None => s.trim().parse::<f64>().map(|v| v / 255.0),
        };
        let rgb: Vec<f64> = args[..3]
            .iter()
            .map(|s| channel(s).map(|v| v.clamp(0.0, 1.0)))
            .collect::<Result<_, _>>()
            .map_err(|_| ())?;
        let a = args.get(3).map_or(Ok(1.0), |s| alpha(s))?;
        return Ok(Some([rgb[0], rgb[1], rgb[2], a]));
    }
    if let Some(args) = function(&v, &["hsl", "hsla"]) {
        if args.len() < 3 {
            return Err(());
        }
        let h = args[0].trim().trim_end_matches("deg").parse::<f64>().map_err(|_| ())?;
        let pct = |s: &str| {
            s.trim().trim_end_matches('%').parse::<f64>().map(|p| (p / 100.0).clamp(0.0, 1.0)).map_err(|_| ())
        };
        let (s, l) = (pct(args[1])?, pct(args[2])?);
        let a = args.get(3).map_or(Ok(1.0), |s| alpha(s))?;
        let [r, g, b] = hsl(h, s, l);
        return Ok(Some([r, g, b, a]));
    }
    let rgb = css_color(&v).ok_or(())?;
    let c = |shift: u32| f64::from((rgb >> shift) & 0xff) / 255.0;
    Ok(Some([c(16), c(8), c(0), 1.0]))
}

fn alpha(s: &str) -> Result<f64, ()> {
    let s = s.trim();
    let a = match s.strip_suffix('%') {
        Some(p) => p.parse::<f64>().map(|p| p / 100.0),
        None => s.parse::<f64>(),
    };
    a.map(|a| a.clamp(0.0, 1.0)).map_err(|_| ())
}

/// The arguments of `name(…)`, split at commas, spaces or a slash.
fn function<'a>(v: &'a str, names: &[&str]) -> Option<Vec<&'a str>> {
    let open = v.find('(')?;
    if !names.contains(&v[..open].trim()) {
        return None;
    }
    let inner = v[open + 1..].trim_end().strip_suffix(')')?;
    Some(inner.split([',', ' ', '/']).map(str::trim).filter(|s| !s.is_empty()).collect())
}

fn hsl(h: f64, s: f64, l: f64) -> [f64; 3] {
    let h = h.rem_euclid(360.0) / 360.0;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |t: f64| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

/// A length in points (CSS pixels, which rich text takes as points), with
/// `em` the font size and `percent` what 100% is. None for what isn't a
/// length.
pub(crate) fn length(value: &str, em: f64, percent: f64) -> Option<f64> {
    let v = value.trim().to_ascii_lowercase();
    if v == "0" {
        return Some(0.0);
    }
    let split = v.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))?;
    let (number, unit) = v.split_at(split);
    let n: f64 = number.parse().ok()?;
    Some(match unit.trim() {
        "px" => n,
        "pt" => n * 4.0 / 3.0,
        "pc" => n * 16.0,
        "in" => n * 96.0,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "em" => n * em,
        "rem" => n * 12.0,
        "ex" | "ch" => n * em / 2.0,
        "%" => n * percent / 100.0,
        _ => return None,
    })
}

/// A `font-size` value, given the parent's size: points.
pub(crate) fn font_size(value: &str, parent: f64) -> Option<f64> {
    let v = value.trim().to_ascii_lowercase();
    let keyword = match v.as_str() {
        "xx-small" => Some(9.0),
        "x-small" => Some(9.0),
        "small" => Some(10.0),
        "medium" => Some(12.0),
        "large" => Some(14.0),
        "x-large" => Some(18.0),
        "xx-large" => Some(24.0),
        "xxx-large" => Some(36.0),
        "smaller" => Some((parent / 1.2).round().max(9.0)),
        "larger" => Some(parent * 1.2),
        _ => None,
    };
    keyword.or_else(|| length(&v, parent, parent)).filter(|s| *s > 0.0)
}

/// Font families from a `font-family` value, unquoted, in order.
pub(crate) fn families(value: &str) -> Vec<String> {
    split_top(value, ',')
        .into_iter()
        .map(|f| f.trim().trim_matches(|c| c == '"' || c == '\'').trim().to_owned())
        .filter(|f| !f.is_empty())
        .collect()
}

/// The parts of a `font` shorthand: style, weight, size, line height and
/// families (each as the longhand's value).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct FontShorthand {
    pub style: Option<String>,
    pub weight: Option<String>,
    pub size: Option<String>,
    pub families: Option<String>,
}

pub(crate) fn font_shorthand(value: &str) -> Option<FontShorthand> {
    let v = value.trim();
    let mut out = FontShorthand::default();
    let mut rest = v;
    loop {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..word_end];
        let lower = word.to_ascii_lowercase();
        match lower.as_str() {
            "italic" | "oblique" => out.style = Some(lower.clone()),
            "bold" | "bolder" | "lighter" | "100" | "200" | "300" | "400" | "500" | "600" | "700" | "800" | "900" => {
                out.weight = Some(lower.clone());
            }
            "normal" | "small-caps" | "condensed" | "expanded" | "semi-condensed" | "semi-expanded" => {}
            _ => {
                // The size (with an optional `/line-height`), then the
                // families.
                let size = word.split('/').next().unwrap_or(word);
                out.size = Some(size.to_owned());
                out.families = Some(rest[word_end..].trim().to_owned()).filter(|f| !f.is_empty());
                return out.size.is_some().then_some(out);
            }
        }
        rest = rest[word_end..].trim_start();
        if rest.is_empty() {
            return None;
        }
    }
}

/// Whether a `font-weight` value is bold, given the parent's.
pub(crate) fn bold(value: &str, parent: bool) -> bool {
    match value.trim().to_ascii_lowercase().as_str() {
        "bold" | "bolder" => true,
        "normal" | "lighter" => false,
        v => v.parse::<f64>().map_or(parent, |w| w >= 600.0),
    }
}

/// The four sides of a `margin`-like shorthand: top, right, bottom, left.
pub(crate) fn sides(value: &str) -> [&str; 4] {
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        [a] => [a, a, a, a],
        [a, b] => [a, b, a, b],
        [a, b, c] => [a, b, c, b],
        [a, b, c, d, ..] => [a, b, c, d],
        [] => ["0", "0", "0", "0"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_and_sheets() {
        assert_eq!(
            declarations("color: red; font: 12px 'A; B', serif ; /* c */ margin:0 !important;;"),
            [
                ("color".into(), "red".into()),
                ("font".into(), "12px 'A; B', serif".into()),
                ("margin".into(), "0".into())
            ]
        );
        let mut sheet = Sheet::default();
        sheet.add("@import url(x.css); p.p1 {margin: 1px} span.s1 { color: blue } @media print { p { color: red } } p, .x { color: green } div p { color: pink } p { font-weight: bold }");
        let p1 = sheet.matching("p", &["p1"]);
        assert_eq!(
            p1,
            [
                ("color".to_string(), "green".to_string()),
                ("font-weight".into(), "bold".into()),
                ("margin".into(), "1px".into())
            ]
        );
        assert_eq!(sheet.matching("span", &["s1"]), [("color".to_string(), "blue".to_string())]);
        assert_eq!(sheet.matching("span", &["x"]), [("color".to_string(), "green".to_string())]);
    }

    #[test]
    fn colors() {
        assert_eq!(color("#f00"), Ok(Some([1.0, 0.0, 0.0, 1.0])));
        assert_eq!(color("#0000ff80").map(|c| c.map(|c| (c[2], (c[3] * 255.0).round()))), Ok(Some((1.0, 128.0))));
        assert_eq!(color("rgb(255, 0, 0)"), Ok(Some([1.0, 0.0, 0.0, 1.0])));
        assert_eq!(color("rgba(0,0,255,0.5)"), Ok(Some([0.0, 0.0, 1.0, 0.5])));
        assert_eq!(color("rgb(0 128 255 / 50%)").map(|c| c.map(|c| c[3])), Ok(Some(0.5)));
        assert_eq!(color("hsl(120, 100%, 50%)"), Ok(Some([0.0, 1.0, 0.0, 1.0])));
        assert_eq!(color("CornflowerBlue").map(|c| c.map(|c| (c[0] * 255.0).round())), Ok(Some(100.0)));
        assert_eq!(color("transparent"), Ok(None));
        assert_eq!(color("nosuch"), Err(()));
    }

    #[test]
    fn lengths_sizes_and_fonts() {
        assert_eq!(length("12px", 12.0, 0.0), Some(12.0));
        assert_eq!(length("11pt", 12.0, 0.0).map(|v| (v * 100.0).round()), Some(1467.0));
        assert_eq!(length("1.5em", 12.0, 0.0), Some(18.0));
        assert_eq!(length("50%", 12.0, 20.0), Some(10.0));
        assert_eq!(length("auto", 12.0, 0.0), None);
        assert_eq!(font_size("150%", 12.0), Some(18.0));
        assert_eq!(font_size("small", 12.0), Some(10.0));
        assert_eq!(font_size("smaller", 12.0), Some(10.0));
        assert_eq!(families(" 'Courier New', \"A B\" ,monospace"), ["Courier New", "A B", "monospace"]);
        let f = font_shorthand("italic bold 14px/20px Georgia, serif").unwrap();
        assert_eq!(
            (f.style.as_deref(), f.weight.as_deref(), f.size.as_deref()),
            (Some("italic"), Some("bold"), Some("14px"))
        );
        assert_eq!(f.families.as_deref(), Some("Georgia, serif"));
        assert_eq!(font_shorthand("12.0px Helvetica").unwrap().families.as_deref(), Some("Helvetica"));
        assert!(bold("700", false) && !bold("500", true) && bold("inherit", true));
        assert_eq!(sides("1px 2px"), ["1px", "2px", "1px", "2px"]);
    }
}
