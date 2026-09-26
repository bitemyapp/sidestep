//! URL strings: RFC 3986's syntax, and the few ways `NSURL` departs from
//! it, all pinned by `conformance/tests/services.rs` against macOS.
//!
//! Strings are first percent-encoded where they hold characters a URL
//! can't (spaces, non-ASCII, a `%` not starting an escape), as
//! `+URLWithString:` does on current macOS. Then they split into
//! components with RFC 3986's appendix B grammar. Resolving a reference
//! against a base follows section 5.2, except that, like `NSURL`, an
//! absolute path is taken as it is and `..` segments that would climb
//! above the root are kept.

use std::ops::Range;

/// Where the components of a URL string are, as byte ranges into it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Parts {
    pub(crate) scheme: Option<Range<usize>>,
    /// Between `//` and the path.
    pub(crate) authority: Option<Range<usize>>,
    pub(crate) user: Option<Range<usize>>,
    pub(crate) password: Option<Range<usize>>,
    /// Without IPv6 brackets.
    pub(crate) host: Option<Range<usize>>,
    /// Digits after `:`, possibly none.
    pub(crate) port: Option<Range<usize>>,
    pub(crate) path: Range<usize>,
    pub(crate) query: Option<Range<usize>>,
    pub(crate) fragment: Option<Range<usize>>,
}

/// Characters allowed anywhere in a URL string, besides `%` escapes.
fn allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=".contains(c)
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

/// Percent-encode what a URL can't hold.
pub(crate) fn encode_invalid(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.char_indices() {
        if c == '%' {
            if bytes.len() > i + 2 && is_hex(bytes[i + 1]) && is_hex(bytes[i + 2]) {
                out.push('%');
            } else {
                out.push_str("%25");
            }
        } else if allowed(c) {
            out.push(c);
        } else {
            push_escaped(&mut out, c);
        }
    }
    out
}

/// Whether a string needs no encoding at all.
pub(crate) fn is_valid(s: &str) -> bool {
    let bytes = s.as_bytes();
    s.char_indices().all(|(i, c)| {
        if c == '%' { bytes.len() > i + 2 && is_hex(bytes[i + 1]) && is_hex(bytes[i + 2]) } else { allowed(c) }
    })
}

fn push_escaped(out: &mut String, c: char) {
    let mut buf = [0u8; 4];
    for b in c.encode_utf8(&mut buf).bytes() {
        out.push('%');
        out.push(char::from_digit(u32::from(b >> 4), 16).unwrap().to_ascii_uppercase());
        out.push(char::from_digit(u32::from(b & 15), 16).unwrap().to_ascii_uppercase());
    }
}

/// Percent-encode `s` for use in a URL component: everything but
/// unreserved characters and `keep`.
pub(crate) fn encode_component(s: &str, keep: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || "-._~".contains(c) || keep.contains(c) {
            out.push(c);
        } else {
            push_escaped(&mut out, c);
        }
    }
    out
}

/// What a path component may keep unescaped.
pub(crate) const PATH_KEEP: &str = "!$&'()*+,;=:@/";
/// Query and fragment.
pub(crate) const QUERY_KEEP: &str = "!$&'()*+,;=:@/?";
/// User and password (a colon too, as on macOS).
pub(crate) const USER_KEEP: &str = "!$&'()*+,;=:";
/// Host names outside IPv6 brackets.
pub(crate) const HOST_KEEP: &str = "!$&'()*+,;=";
/// A query item's name or value: a query's set less `&` and `=`.
pub(crate) const ITEM_KEEP: &str = "!$'()*+,;:@/?";

/// Decode `%XX` escapes; `None` if the result isn't UTF-8 or an escape is
/// malformed.
pub(crate) fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let text = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(text, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Split a (valid) URL string into components; `None` for strings no
/// URL can be made of (a port that isn't a number, an unclosed IPv6
/// bracket).
pub(crate) fn parse(s: &str) -> Option<Parts> {
    let bytes = s.as_bytes();
    let mut parts = Parts::default();
    let mut at = 0;
    // scheme ":" — a colon before any of "/?#", after a valid scheme.
    if let Some(colon) = s.find(|c| ":/?#".contains(c)).filter(|&i| bytes[i] == b':') {
        let scheme = &s[..colon];
        let valid = scheme.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
            && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b));
        if valid {
            parts.scheme = Some(0..colon);
            at = colon + 1;
        }
    }
    if s[at..].starts_with("//") {
        let start = at + 2;
        let end = s[start..].find(['/', '?', '#']).map_or(s.len(), |i| start + i);
        parts.authority = Some(start..end);
        authority(s, start..end, &mut parts)?;
        at = end;
    }
    let path_end = s[at..].find(['?', '#']).map_or(s.len(), |i| at + i);
    parts.path = at..path_end;
    at = path_end;
    if s[at..].starts_with('?') {
        let end = s[at..].find('#').map_or(s.len(), |i| at + i);
        parts.query = Some(at + 1..end);
        at = end;
    }
    if s[at..].starts_with('#') {
        parts.fragment = Some(at + 1..s.len());
    }
    Some(parts)
}

fn authority(s: &str, range: Range<usize>, parts: &mut Parts) -> Option<()> {
    let text = &s[range.clone()];
    let mut host_start = range.start;
    if let Some(at) = text.rfind('@') {
        let info = range.start..range.start + at;
        match s[info.clone()].find(':') {
            Some(colon) => {
                parts.user = Some(info.start..info.start + colon);
                parts.password = Some(info.start + colon + 1..info.end);
            }
            None => parts.user = Some(info),
        }
        host_start = range.start + at + 1;
    }
    let host_text = &s[host_start..range.end];
    let (host, rest) = if host_text.starts_with('[') {
        let close = host_text.find(']')?;
        (host_start + 1..host_start + close, host_start + close + 1)
    } else {
        let end = host_text.rfind(':').map_or(range.end, |i| host_start + i);
        (host_start..end, end)
    };
    parts.host = Some(host);
    if rest < range.end {
        if s.as_bytes()[rest] != b':' {
            return None;
        }
        let port = rest + 1..range.end;
        if !s[port.clone()].bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        parts.port = Some(port);
    }
    Some(())
}

/// Remove `.` and `..` segments, keeping `..` that would climb above the
/// root, as `NSURL` does.
pub(crate) fn remove_dots(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len() - 1;
    let mut trailing = false;
    for (i, segment) in segments.iter().enumerate() {
        if i == 0 && absolute {
            continue;
        }
        match *segment {
            "." => trailing = i == last,
            ".." => {
                if out.last().is_some_and(|s| *s != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
                trailing = i == last;
            }
            segment => {
                out.push(segment);
                trailing = false;
            }
        }
    }
    let mut result = String::with_capacity(path.len());
    if absolute {
        result.push('/');
    }
    result.push_str(&out.join("/"));
    if trailing && !result.ends_with('/') {
        result.push('/');
    }
    result
}

/// `-standardizedURL`'s path: `.` segments go, `..` segments take the
/// segment before them (or go, when there is none), and a path ending in
/// `..` loses the slash before the segment it took.
pub(crate) fn standardize_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let segments: Vec<&str> = path.split('/').skip(usize::from(absolute)).collect();
    let last = segments.len().saturating_sub(1);
    let mut out: Vec<&str> = Vec::with_capacity(segments.len());
    let mut popped_last = false;
    for (i, segment) in segments.iter().enumerate() {
        match *segment {
            "." if i == last => out.push(""),
            "." => {}
            ".." => {
                let popped = out.pop().is_some();
                popped_last = popped && i == last;
            }
            segment => out.push(segment),
        }
    }
    if absolute && popped_last && out.is_empty() {
        return String::new();
    }
    let joined = out.join("/");
    if absolute { format!("/{joined}") } else { joined }
}

/// Resolve `reference` against `base` (both valid URL strings).
pub(crate) fn resolve(base: &str, reference: &str) -> String {
    let (Some(b), Some(r)) = (parse(base), parse(reference)) else { return reference.to_string() };
    let part = |s: &str, r: &Option<Range<usize>>| r.clone().map(|r| s[r].to_string());
    if r.scheme.is_some() {
        return reference.to_string();
    }
    let mut out = String::with_capacity(base.len() + reference.len());
    if let Some(scheme) = &b.scheme {
        out.push_str(&base[scheme.clone()]);
        out.push(':');
    }
    let (authority, path, query);
    if r.authority.is_some() {
        authority = part(reference, &r.authority);
        path = reference[r.path.clone()].to_string();
        query = part(reference, &r.query);
    } else {
        authority = part(base, &b.authority);
        let r_path = &reference[r.path.clone()];
        if r_path.is_empty() {
            path = base[b.path.clone()].to_string();
            query = part(reference, &r.query).or_else(|| part(base, &b.query));
        } else if r_path.starts_with('/') {
            path = r_path.to_string();
            query = part(reference, &r.query);
        } else {
            let b_path = &base[b.path.clone()];
            let merged = if b.authority.is_some() && b_path.is_empty() {
                format!("/{r_path}")
            } else {
                match b_path.rfind('/') {
                    Some(i) => format!("{}{r_path}", &b_path[..=i]),
                    None => r_path.to_string(),
                }
            };
            path = remove_dots(&merged);
            query = part(reference, &r.query);
        }
    }
    if let Some(authority) = authority {
        out.push_str("//");
        out.push_str(&authority);
    }
    out.push_str(&path);
    if let Some(query) = query {
        out.push('?');
        out.push_str(&query);
    }
    if let Some(fragment) = part(reference, &r.fragment) {
        out.push('#');
        out.push_str(&fragment);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_3986_examples_as_nsurl_resolves_them() {
        let base = "http://a/b/c/d;p?q";
        for (reference, expected) in [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            (";x", "http://a/b/c/;x"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../..", "http://a/"),
            ("../../../g", "http://a/../g"),
            ("/./g", "http://a/./g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g;x=1/../y", "http://a/b/c/y"),
            ("g?y/../x", "http://a/b/c/g?y/../x"),
            ("g#s/../x", "http://a/b/c/g#s/../x"),
        ] {
            assert_eq!(resolve(base, reference), expected, "{reference}");
        }
    }

    #[test]
    fn parts() {
        let s = "http://user:pw@[::1]:8080/p%20q?x=1#f";
        let p = parse(s).unwrap();
        assert_eq!(&s[p.host.unwrap()], "::1");
        assert_eq!(&s[p.port.unwrap()], "8080");
        assert_eq!(&s[p.user.unwrap()], "user");
        assert_eq!(&s[p.path], "/p%20q");
        assert!(parse("http://host:abc/p").is_none());
        assert_eq!(encode_invalid("a b%zz/é"), "a%20b%25zz/%C3%A9");
    }

    #[test]
    fn standardized_paths() {
        for (path, expected) in [
            ("/a/b/..", "/a"),
            ("/a/b/../", "/a/"),
            ("/a/b/../c", "/a/c"),
            ("a/b/..", "a"),
            ("/..", "/"),
            ("..", ""),
            (".", ""),
            ("a/.", "a/"),
            ("a/..", ""),
            ("/a/..", ""),
            ("/a/../", "/"),
            ("/./", "/"),
            ("../..", ""),
            ("/../..", "/"),
            ("a//b/../c", "a//c"),
            ("/a/./b/../c/", "/a/c/"),
            ("../x/./y", "x/y"),
        ] {
            assert_eq!(standardize_path(path), expected, "{path}");
        }
    }
}
