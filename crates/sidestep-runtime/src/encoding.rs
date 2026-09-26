//! Splitting Objective-C type encodings such as `v24@0:8@16` into their
//! component types (`v`, `@`, `:`, `@`), dropping frame offsets.

/// Type qualifiers that may prefix a type: const, in, inout, out, bycopy,
/// byref, oneway, atomic, complex.
const QUALIFIERS: &[u8] = b"rnNoORVAj";

/// Split a method's type encoding into its return type and argument types.
pub(crate) fn split(mut s: &[u8]) -> Vec<&[u8]> {
    let mut types = Vec::new();
    while !s.is_empty() {
        let len = type_len(s);
        if len == 0 {
            break;
        }
        types.push(&s[..len]);
        s = &s[len..];
        // Skip the frame offset that may follow each type.
        let digits = s.iter().take_while(|&&b| b == b'-' || b.is_ascii_digit()).count();
        s = &s[digits..];
    }
    types
}

/// The length of the single type at the start of `s`.
pub(crate) fn type_len(s: &[u8]) -> usize {
    let qualifiers = s.iter().take_while(|b| QUALIFIERS.contains(b)).count();
    let rest = &s[qualifiers..];
    let Some(&first) = rest.first() else { return 0 };
    let body = match first {
        b'^' => 1 + type_len(&rest[1..]),
        b'[' | b'{' | b'(' => bracketed_len(rest),
        b'b' => 1 + rest[1..].iter().take_while(|b| b.is_ascii_digit()).count(),
        b'@' => {
            let mut len = 1;
            match rest.get(1) {
                // `@"NSString"`: a class name.
                Some(b'"') => {
                    len += 1 + rest[2..].iter().position(|&b| b == b'"').map_or(rest.len() - 2, |p| p + 1);
                }
                // `@?`: a block, optionally followed by `<signature>`.
                Some(b'?') => {
                    len += 1;
                    if rest.get(2) == Some(&b'<') {
                        len += bracketed_len(&rest[2..]);
                    }
                }
                _ => {}
            }
            len
        }
        _ => 1,
    };
    qualifiers + body.min(rest.len())
}

/// The length of a bracketed type (`[...]`, `{...}`, `(...)`, `<...>`),
/// including nested brackets and quoted field names.
pub(crate) fn bracketed_len(s: &[u8]) -> usize {
    let mut depth = 0usize;
    let mut quoted = false;
    for (i, &b) in s.iter().enumerate() {
        match b {
            b'"' => quoted = !quoted,
            _ if quoted => {}
            b'[' | b'{' | b'(' | b'<' => depth += 1,
            b']' | b'}' | b')' | b'>' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    s.len()
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn splits_with_and_without_offsets() {
        assert_eq!(split(b"v24@0:8@16"), [&b"v"[..], b"@", b":", b"@"]);
        assert_eq!(split(b"@@:"), [&b"@"[..], b"@", b":"]);
        assert_eq!(split(b"Vv@:"), [&b"Vv"[..], b"@", b":"]);
    }

    #[test]
    fn splits_compound_types() {
        assert_eq!(
            split(b"{CGRect={CGPoint=dd}{CGSize=dd}}@:^{Foo=i}[4c]@\"NSString\"@?<v@?>b3"),
            [
                &b"{CGRect={CGPoint=dd}{CGSize=dd}}"[..],
                b"@",
                b":",
                b"^{Foo=i}",
                b"[4c]",
                b"@\"NSString\"",
                b"@?<v@?>",
                b"b3",
            ]
        );
    }
}
