//! A tolerant HTML tokenizer: tags with their attributes, text with its
//! character references decoded, comments and doctypes dropped. It follows
//! the HTML standard's tokenizer where pasted HTML needs it (attribute
//! values quoted or not, raw text in `script` and `style`, escapable raw
//! text in `title` and `textarea`, references with or without their `;`)
//! and never fails: what isn't markup is text.

use super::tables::entity;

/// A token of HTML.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Token {
    /// A start tag: its name (lowercase), attributes (names lowercase,
    /// values decoded) and whether it closed itself (`<br/>`).
    Start {
        name: String,
        attrs: Vec<(String, String)>,
        closed: bool,
    },
    End {
        name: String,
    },
    Text(String),
}

impl Token {
    /// A start tag's attribute.
    pub fn attr(&self, name: &str) -> Option<&str> {
        match self {
            Token::Start { attrs, .. } => attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()),
            _ => None,
        }
    }
}

/// The tokens of `html`.
pub(crate) fn tokens(html: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let b = html.as_bytes();
    let mut at = 0;
    let mut text_start = 0;
    let flush = |out: &mut Vec<Token>, from: usize, to: usize| {
        if to > from {
            out.push(Token::Text(decode(&html[from..to], false)));
        }
    };
    while at < b.len() {
        if b[at] != b'<' {
            at += 1;
            continue;
        }
        let rest = &html[at..];
        if rest.starts_with("<!--") {
            flush(&mut out, text_start, at);
            at = rest.find("-->").map_or(b.len(), |i| at + i + 3);
            text_start = at;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            flush(&mut out, text_start, at);
            at = rest.find('>').map_or(b.len(), |i| at + i + 1);
            text_start = at;
            continue;
        }
        let end_tag = rest.starts_with("</");
        let name_start = at + if end_tag { 2 } else { 1 };
        if !b.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            // `<` that starts no tag is text.
            at += 1;
            continue;
        }
        flush(&mut out, text_start, at);
        let (token, next) = tag(html, name_start, end_tag);
        at = next;
        text_start = at;
        if let Token::Start { name, closed: false, .. } = &token {
            let raw =
                matches!(name.as_str(), "script" | "style" | "title" | "textarea" | "xmp" | "noembed" | "noframes");
            if raw {
                let escapable = matches!(name.as_str(), "title" | "textarea");
                let close = format!("</{name}");
                let end = find_ci(&html[at..], &close).map_or(b.len(), |i| at + i);
                let name = name.clone();
                out.push(token);
                if end > at {
                    let text = &html[at..end];
                    out.push(Token::Text(if escapable { decode(text, false) } else { text.to_owned() }));
                }
                out.push(Token::End { name });
                at = html[end..].find('>').map_or(b.len(), |i| end + i + 1);
                text_start = at;
                continue;
            }
        }
        out.push(token);
    }
    flush(&mut out, text_start, b.len());
    out
}

/// Where `needle` (lowercase ASCII) is in `haystack`, ignoring case.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    (0..h.len().saturating_sub(n.len() - 1)).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// A tag whose name starts at `at`: the token and where parsing goes on.
fn tag(html: &str, mut at: usize, end_tag: bool) -> (Token, usize) {
    let b = html.as_bytes();
    let start = at;
    while at < b.len() && !b[at].is_ascii_whitespace() && b[at] != b'>' && b[at] != b'/' {
        at += 1;
    }
    let name = html[start..at].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut closed = false;
    loop {
        while at < b.len() && (b[at].is_ascii_whitespace() || (b[at] == b'/' && b.get(at + 1) != Some(&b'>'))) {
            at += 1;
        }
        match b.get(at) {
            None => break,
            Some(b'>') => {
                at += 1;
                break;
            }
            Some(b'/') => {
                closed = true;
                at += 2;
                break;
            }
            _ => {}
        }
        let name_start = at;
        while at < b.len() && !b[at].is_ascii_whitespace() && !matches!(b[at], b'=' | b'>') && b[at] != b'/' {
            at += 1;
        }
        let attr = html[name_start..at].to_ascii_lowercase();
        while at < b.len() && b[at].is_ascii_whitespace() {
            at += 1;
        }
        let mut value = String::new();
        if b.get(at) == Some(&b'=') {
            at += 1;
            while at < b.len() && b[at].is_ascii_whitespace() {
                at += 1;
            }
            match b.get(at) {
                Some(&q @ (b'"' | b'\'')) => {
                    let v_start = at + 1;
                    let v_end = html[v_start..].find(char::from(q)).map_or(b.len(), |i| v_start + i);
                    value = decode(&html[v_start..v_end], true);
                    at = (v_end + 1).min(b.len());
                }
                _ => {
                    let v_start = at;
                    while at < b.len() && !b[at].is_ascii_whitespace() && b[at] != b'>' {
                        at += 1;
                    }
                    value = decode(&html[v_start..at], true);
                }
            }
        }
        if !attr.is_empty() && !attrs.iter().any(|(n, _): &(String, String)| *n == attr) {
            attrs.push((attr, value));
        }
    }
    let token = if end_tag { Token::End { name } } else { Token::Start { name, attrs, closed } };
    (token, at)
}

/// Text with its character references decoded. In an attribute value, a
/// named reference without its `;` followed by `=` or an alphanumeric
/// stays as written, as the standard has it.
pub(crate) fn decode(text: &str, attribute: bool) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        match reference(rest, attribute) {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character a reference at the start of `s` (at its `&`) stands for,
/// and how long it is.
fn reference(s: &str, attribute: bool) -> Option<(char, usize)> {
    let b = s.as_bytes();
    if b.get(1) == Some(&b'#') {
        let (hex, start) = if matches!(b.get(2), Some(b'x' | b'X')) { (true, 3) } else { (false, 2) };
        let mut end = start;
        while end < b.len() && (if hex { b[end].is_ascii_hexdigit() } else { b[end].is_ascii_digit() }) {
            end += 1;
        }
        if end == start {
            return None;
        }
        let n = u32::from_str_radix(&s[start..end.min(start + 8)], if hex { 16 } else { 10 }).unwrap_or(0xFFFD);
        let len = if b.get(end) == Some(&b';') { end + 1 } else { end };
        return Some((numeric(n), len));
    }
    let mut end = 1;
    while end < b.len() && b[end].is_ascii_alphanumeric() && end < 34 {
        end += 1;
    }
    if b.get(end) == Some(&b';') {
        return entity(&s[1..end]).map(|c| (c, end + 1));
    }
    // Without a `;`: the longest known name that starts it (as `&amp` or
    // `&copy2`), outside attributes.
    if attribute {
        return None;
    }
    (3..=end).rev().find_map(|e| entity(&s[1..e]).map(|c| (c, e)))
}

/// The character a numeric reference names, with the standard's
/// replacements (Windows-1252's characters for 0x80 to 0x9F).
fn numeric(n: u32) -> char {
    if (0x80..0xA0).contains(&n) {
        return super::tables::CodePage::Cp1252.decode(n as u8);
    }
    match n {
        0 => char::REPLACEMENT_CHARACTER,
        _ => char::from_u32(n).unwrap_or(char::REPLACEMENT_CHARACTER),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(name: &str, attrs: &[(&str, &str)], closed: bool) -> Token {
        Token::Start {
            name: name.into(),
            attrs: attrs.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect(),
            closed,
        }
    }

    #[test]
    fn tags_text_and_attributes() {
        let t = tokens("<P Class=p1 style=\"color: red\">a &amp; b<BR/>c</p><!-- x --><img alt='&lt;x&gt;' src=a.png>");
        assert_eq!(
            t,
            [
                start("p", &[("class", "p1"), ("style", "color: red")], false),
                Token::Text("a & b".into()),
                start("br", &[], true),
                Token::Text("c".into()),
                Token::End { name: "p".into() },
                start("img", &[("alt", "<x>"), ("src", "a.png")], false),
            ]
        );
        assert_eq!(t[0].attr("style"), Some("color: red"));
    }

    #[test]
    fn raw_text_and_stray_brackets() {
        let t = tokens("<style>p { color: <b> }</style>1 < 2 <3<script>if (a<b) x()</script>");
        assert_eq!(
            t,
            [
                start("style", &[], false),
                Token::Text("p { color: <b> }".into()),
                Token::End { name: "style".into() },
                Token::Text("1 < 2 <3".into()),
                start("script", &[], false),
                Token::Text("if (a<b) x()".into()),
                Token::End { name: "script".into() },
            ]
        );
        assert_eq!(tokens("<!DOCTYPE html><title>A &amp; B</title>")[1], Token::Text("A & B".into()));
    }

    #[test]
    fn references() {
        assert_eq!(
            decode("&lt;&#65;&#x263a;&#X263A;&eacute;&copy2 &hellip;&unknown; & &#150;", false),
            "<A☺☺é©2 …&unknown; & –"
        );
        assert_eq!(decode("?a=1&copy=2", true), "?a=1&copy=2");
    }
}
