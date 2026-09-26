//! ICU pattern syntax, rewritten for fancy-regex.
//!
//! The two agree on most of the syntax. Where they don't, the pattern is
//! rewritten as it is read:
//!
//! - Lines. ICU ends a line at LF, CR, CR LF, VT, FF, NEL, LINE SEPARATOR
//!   and PARAGRAPH SEPARATOR (only LF with `UseUnixLineSeparators`); Rust's
//!   engines know LF alone. So `.`, `^`, `$` and `\Z` become explicit
//!   classes and look-arounds over ICU's terminators, and the `m` and `s`
//!   flags, set by option or inline as `(?m)` or `(?s:…)`, are followed
//!   group by group here rather than handed on. In multi-line mode `^`
//!   doesn't match at the very end of the text, `$` matches before every
//!   terminator, and neither matches between the CR and LF of a CR LF.
//! - Escapes Rust lacks or reads differently: `\R`, `\h`, `\v` (vertical
//!   space in ICU, the VT character in Rust), `\X` (a grapheme cluster, here
//!   approximated), `\e`, `\cX`, octal `\0ooo` and `\Q…\E` quoting.
//! - POSIX classes such as `[[:alpha:]]`, which are Unicode properties in
//!   ICU and ASCII in Rust, and `(?#…)` comments.
//!
//! Not translated: character names (`\N{…}`, which needs a name table),
//! `\G`, and full case folding (ICU's `(?i)straße` matches `STRASSE`; Rust
//! folds one character to one). `(?w)` and `UseUnicodeWordBoundaries` are
//! accepted, with Rust's Unicode `\b`.
//!
//! Reading the pattern also notes what matching over part of a string must
//! know about it ([`Traits`]).

/// `NSRegularExpressionCaseInsensitive` and the other options.
pub(crate) const CASE_INSENSITIVE: usize = 1 << 0;
pub(crate) const ALLOW_COMMENTS: usize = 1 << 1;
pub(crate) const IGNORE_METACHARACTERS: usize = 1 << 2;
pub(crate) const DOT_MATCHES_LINE_SEPARATORS: usize = 1 << 3;
pub(crate) const ANCHORS_MATCH_LINES: usize = 1 << 4;
pub(crate) const USE_UNIX_LINE_SEPARATORS: usize = 1 << 5;

/// ICU's line terminators other than CR, as class members.
const TERMINATORS_BUT_CR: &str = r"\n\x0B\x0C\x{85}\x{2028}\x{2029}";
/// The terminators other than CR and LF.
const TERMINATORS_BUT_CRLF: &str = r"\x0B\x0C\x{85}\x{2028}\x{2029}";
/// ICU's `\v`, as class members.
const VERTICAL: &str = r"\n\x0B\x0C\r\x{85}\x{2028}\x{2029}";
/// ICU's `\h`, as class members.
const HORIZONTAL: &str = r"\t\p{Zs}";

/// What matching over part of a string, and reporting on it, must know
/// about a pattern.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Traits {
    /// It looks behind (`(?<=…)`, `(?<!…)`).
    pub looks_behind: bool,
    /// It anchors to the start (`^`, `\A`).
    pub anchors_start: bool,
    /// It asks where a word starts or ends (`\b`, `\B`).
    pub word_bounds: bool,
    /// It anchors to the end (`$`, `\Z`, `\z`).
    pub anchors_end: bool,
    /// It looks ahead (`(?=…)`, `(?!…)`).
    pub looks_ahead: bool,
    /// Its last element could have read on had the text gone on: a greedy
    /// quantifier that isn't bounded, or an assertion about what follows.
    /// This stands in for ICU's record of reading past the end.
    pub open_end: bool,
    /// It can match only at the start of the text (it begins with `^` or
    /// `\A` outside multi-line mode, with no alternatives).
    pub anchored: bool,
}

/// A pattern rewritten for fancy-regex, and its traits.
pub(crate) struct Translated {
    pub pattern: String,
    pub traits: Traits,
}

/// The flags that change how the pattern is rewritten.
#[derive(Clone, Copy)]
struct Flags {
    multi: bool,
    dot_all: bool,
    extended: bool,
    unix: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Group {
    /// Captures or just groups; its contents are an element.
    Plain,
    LookAhead,
    LookBehind,
}

/// A group being read: the flags to restore after it, and whether any of
/// its alternatives, and the one being read, end open.
struct Level {
    saved: Flags,
    kind: Group,
    alt_open: bool,
    cur_open: bool,
}

struct Translator {
    chars: Vec<char>,
    i: usize,
    out: String,
    flags: Flags,
    levels: Vec<Level>,
    traits: Traits,
    /// Whether anything that consumes or asserts has been read.
    started: bool,
}

/// Rewrite an ICU pattern under `options` for fancy-regex.
pub(crate) fn translate(pattern: &str, options: usize) -> Translated {
    let mut out = String::with_capacity(pattern.len() + 16);
    if options & CASE_INSENSITIVE != 0 {
        out.push_str("(?i)");
    }
    if options & ALLOW_COMMENTS != 0 {
        out.push_str("(?x)");
    }
    if options & IGNORE_METACHARACTERS != 0 {
        out.push_str(&fancy_regex::escape(pattern));
        return Translated { pattern: out, traits: Traits::default() };
    }
    let flags = Flags {
        multi: options & ANCHORS_MATCH_LINES != 0,
        dot_all: options & DOT_MATCHES_LINE_SEPARATORS != 0,
        extended: options & ALLOW_COMMENTS != 0,
        unix: options & USE_UNIX_LINE_SEPARATORS != 0,
    };
    let mut t = Translator {
        chars: pattern.chars().collect(),
        i: 0,
        out,
        flags,
        levels: vec![Level { saved: flags, kind: Group::Plain, alt_open: false, cur_open: false }],
        traits: Traits::default(),
        started: false,
    };
    t.run();
    let top = &t.levels[0];
    t.traits.open_end = top.alt_open || top.cur_open;
    Translated { pattern: t.out, traits: t.traits }
}

impl Translator {
    fn peek(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).copied()
    }

    fn level(&mut self) -> &mut Level {
        self.levels.last_mut().expect("the top level")
    }

    /// An element that consumes text was read: nothing after it is open.
    fn atom(&mut self) {
        self.level().cur_open = false;
        self.started = true;
    }

    /// An assertion about what follows was read.
    fn peeks(&mut self) {
        self.level().cur_open = true;
        self.started = true;
    }

    fn run(&mut self) {
        while let Some(c) = self.peek(0) {
            match c {
                '\\' => self.escape(),
                '[' => {
                    let class = self.class();
                    self.out.push_str(&class);
                    self.atom();
                }
                '(' => self.open_group(),
                ')' => self.close_group(),
                '|' => {
                    self.i += 1;
                    self.out.push('|');
                    let level = self.level();
                    level.alt_open |= level.cur_open;
                    level.cur_open = false;
                    if self.levels.len() == 1 {
                        self.traits.anchored = false;
                        self.started = true;
                    }
                }
                '*' | '+' | '?' => {
                    self.i += 1;
                    self.out.push(c);
                    self.quantified(true);
                }
                '{' => self.braces(),
                '^' => {
                    self.i += 1;
                    self.caret();
                }
                '$' => {
                    self.i += 1;
                    self.dollar(self.flags.multi);
                }
                '.' => {
                    self.i += 1;
                    let dot = self.dot();
                    self.out.push_str(&dot);
                    self.atom();
                }
                '#' if self.flags.extended => {
                    // A comment, to the end of the line.
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                c if self.flags.extended && c.is_whitespace() => {
                    self.i += 1;
                    self.out.push(c);
                }
                _ => {
                    self.i += 1;
                    self.out.push(c);
                    self.atom();
                }
            }
        }
    }

    /// After a quantifier: a lazy one (`?` after it) stops as soon as it
    /// can, a possessive one (`+`) or a greedy `open` one reads on.
    fn quantified(&mut self, open: bool) {
        let lazy = match self.peek(0) {
            Some('?') => {
                self.i += 1;
                self.out.push('?');
                true
            }
            Some('+') => {
                self.i += 1;
                self.out.push('+');
                false
            }
            _ => false,
        };
        self.level().cur_open = open && !lazy;
    }

    /// `{n}`, `{n,}` or `{n,m}`, or a literal brace.
    fn braces(&mut self) {
        let rest: String = self.chars[self.i..].iter().take_while(|&&c| c != '}').collect();
        let body = &rest[1..];
        let (n, m) = match body.split_once(',') {
            Some((n, m)) => (n, Some(m)),
            None => (body, None),
        };
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let valid = self.i + rest.chars().count() < self.chars.len()
            && digits(n)
            && m.is_none_or(|m| m.is_empty() || digits(m));
        if !valid {
            self.i += 1;
            self.out.push_str("\\{");
            self.atom();
            return;
        }
        self.i += rest.chars().count() + 1;
        self.out.push_str(&rest);
        self.out.push('}');
        // Open unless it counts exactly, or up to a bound.
        self.quantified(m.is_some_and(str::is_empty));
    }

    /// Whether nothing has been read yet at the top level, so an anchor
    /// here anchors the whole pattern.
    fn at_start(&self) -> bool {
        !self.started && self.levels.len() == 1
    }

    fn caret(&mut self) {
        if self.flags.multi {
            if self.flags.unix {
                self.out.push_str(r"(?:\A|(?<=\n)(?!\z))");
            } else {
                // After a terminator (not between CR and LF), and not at the
                // very end.
                self.out.push_str(&format!(r"(?:\A|(?:(?<=[{TERMINATORS_BUT_CR}])|(?<=\r)(?!\n))(?!\z))"));
            }
        } else {
            self.out.push('^');
            if self.at_start() {
                self.traits.anchored = true;
            }
        }
        self.traits.anchors_start = true;
        self.started = true;
    }

    /// `$`, or with `multi` false also `\Z`.
    fn dollar(&mut self, multi: bool) {
        if self.flags.unix {
            self.out.push_str(if multi { r"(?:\z|(?=\n))" } else { r"(?:\z|(?=\n\z))" });
        } else if multi {
            // Before any terminator, but not between CR and LF.
            self.out.push_str(&format!(
                r"(?:\z|(?=\r)|(?<!\r)(?=[{TERMINATORS_BUT_CR}])|(?<=\r)(?=[{TERMINATORS_BUT_CRLF}]))"
            ));
        } else {
            // At the end, or before one terminator (CR LF counting as one)
            // that ends the text.
            self.out.push_str(&format!(
                r"(?:\z|(?=\r\n?\z)|(?<!\r)(?=[{TERMINATORS_BUT_CR}]\z)|(?<=\r)(?=[{TERMINATORS_BUT_CRLF}]\z))"
            ));
        }
        self.traits.anchors_end = true;
        self.peeks();
    }

    fn dot(&self) -> String {
        if self.flags.dot_all {
            "(?s:.)".to_owned()
        } else if self.flags.unix {
            r"[^\n]".to_owned()
        } else {
            format!(r"[^\r{TERMINATORS_BUT_CR}]")
        }
    }

    fn escape(&mut self) {
        let Some(n) = self.peek(1) else {
            // A trailing backslash: let the engine report it.
            self.i += 1;
            self.out.push('\\');
            return;
        };
        self.i += 2;
        match n {
            'Q' => {
                // \Q…\E quotes everything up to \E.
                let mut literal = String::new();
                while let Some(c) = self.peek(0) {
                    if c == '\\' && self.peek(1) == Some('E') {
                        self.i += 2;
                        break;
                    }
                    literal.push(c);
                    self.i += 1;
                }
                self.out.push_str(&fancy_regex::escape(&literal));
                if !literal.is_empty() {
                    self.atom();
                }
            }
            'E' => {}
            'Z' => self.dollar(false),
            'z' => {
                self.out.push_str(r"\z");
                self.traits.anchors_end = true;
                self.peeks();
            }
            'A' => {
                self.out.push_str(r"\A");
                if self.at_start() {
                    self.traits.anchored = true;
                }
                self.traits.anchors_start = true;
                self.started = true;
            }
            'b' | 'B' => {
                self.out.push('\\');
                self.out.push(n);
                self.traits.word_bounds = true;
                self.peeks();
            }
            'R' => {
                self.out.push_str(&format!(r"(?:\r\n|[{VERTICAL}])"));
                self.atom();
            }
            'X' => {
                // A grapheme cluster, near enough: CR LF, a flag, or a
                // character with the marks and joined characters after it.
                self.out.push_str(
                    r"(?:\r\n|\p{Regional_Indicator}{2}|(?s:.)[\p{M}\p{Emoji_Modifier}]*(?:\x{200D}(?s:.)[\p{M}\p{Emoji_Modifier}]*)*)",
                );
                // It reads on to see whether the cluster goes on.
                self.peeks();
            }
            'h' | 'H' | 'v' | 'V' => {
                let set = self.class_escape(n).expect("a class escape");
                self.out.push('[');
                self.out.push_str(&set);
                self.out.push(']');
                self.atom();
            }
            _ => {
                let lit = self.literal_escape(n);
                self.out.push_str(&lit);
                self.atom();
            }
        }
    }

    /// `\h`, `\H`, `\v` or `\V` as class members.
    fn class_escape(&self, n: char) -> Option<String> {
        Some(match n {
            'h' => HORIZONTAL.to_owned(),
            'H' => format!("[^{HORIZONTAL}]"),
            'v' => VERTICAL.to_owned(),
            'V' => format!("[^{VERTICAL}]"),
            _ => return None,
        })
    }

    /// The rest of an escape of one character, `\` and `n` already read:
    /// rewritten where Rust spells it differently, otherwise copied with
    /// what belongs to it (a `\p{…}` name, hex digits).
    fn literal_escape(&mut self, n: char) -> String {
        match n {
            'e' => r"\x1B".to_owned(),
            'c' => match self.peek(0) {
                Some(c) if c.is_ascii() => {
                    self.i += 1;
                    format!(r"\x{{{:X}}}", (c as u32) & 0x1F)
                }
                _ => r"\c".to_owned(),
            },
            '0' => {
                // Up to three octal digits.
                let mut v = 0u32;
                let mut k = 0;
                while k < 3
                    && let Some(d) = self.peek(0).and_then(|c| c.to_digit(8))
                {
                    v = v * 8 + d;
                    self.i += 1;
                    k += 1;
                }
                format!(r"\x{{{v:X}}}")
            }
            'p' | 'P' | 'x' | 'N' if self.peek(0) == Some('{') => {
                let mut s = format!("\\{n}");
                while let Some(c) = self.peek(0) {
                    self.i += 1;
                    s.push(c);
                    if c == '}' {
                        break;
                    }
                }
                s
            }
            _ => format!("\\{n}"),
        }
    }

    /// A character class, from its `[`, rewritten: POSIX classes become
    /// Unicode properties, and `\h` and `\v` their members.
    fn class(&mut self) -> String {
        let mut out = String::from("[");
        self.i += 1;
        // `[:alpha:]` standing alone is itself a set in ICU.
        if self.peek(0) == Some(':')
            && let Some(set) = self.posix()
        {
            out.push_str(&set);
            out.push(']');
            // The closing `]` of `[:alpha:]` was the class's own.
            return out;
        }
        if self.peek(0) == Some('^') {
            self.i += 1;
            out.push('^');
        }
        // A `]` first is a member.
        if self.peek(0) == Some(']') {
            self.i += 1;
            out.push_str(r"\]");
        }
        while let Some(c) = self.peek(0) {
            match c {
                ']' => {
                    self.i += 1;
                    out.push(']');
                    return out;
                }
                '[' if self.peek(1) == Some(':') => {
                    self.i += 1;
                    match self.posix() {
                        Some(set) => out.push_str(&set),
                        None => out.push_str(r"\["),
                    }
                }
                '[' => {
                    let inner = self.class();
                    out.push_str(&inner);
                }
                '\\' => {
                    let Some(n) = self.peek(1) else {
                        self.i += 1;
                        out.push('\\');
                        continue;
                    };
                    self.i += 2;
                    match self.class_escape(n) {
                        Some(set) => out.push_str(&set),
                        None if n == 'Q' => {
                            while let Some(c) = self.peek(0) {
                                if c == '\\' && self.peek(1) == Some('E') {
                                    self.i += 2;
                                    break;
                                }
                                self.i += 1;
                                out.push_str(&fancy_regex::escape(&c.to_string()));
                            }
                        }
                        None => {
                            let lit = self.literal_escape(n);
                            out.push_str(&lit);
                        }
                    }
                }
                _ => {
                    self.i += 1;
                    out.push(c);
                }
            }
        }
        // Unclosed: let the engine report it.
        out
    }

    /// A POSIX class `:name:]` (the `[` read, `self.i` at the `:`), as class
    /// members, or `None` (reading nothing) if it isn't one.
    fn posix(&mut self) -> Option<String> {
        let rest: String = self.chars[self.i + 1..].iter().take_while(|&&c| c != ':' && c != ']').collect();
        let end = self.i + 1 + rest.chars().count();
        if self.chars.get(end) != Some(&':') || self.chars.get(end + 1) != Some(&']') {
            return None;
        }
        let (negated, name) = match rest.strip_prefix('^') {
            Some(name) => (true, name),
            None => (false, rest.as_str()),
        };
        let members = match name {
            "alpha" => r"\p{Alphabetic}",
            "lower" => r"\p{Lowercase}",
            "upper" => r"\p{Uppercase}",
            "punct" => r"\p{P}",
            "digit" => r"\p{Nd}",
            "xdigit" => r"\p{Nd}\p{Hex_Digit}",
            "alnum" => r"\p{Alphabetic}\p{Nd}",
            "space" => r"\p{White_Space}",
            "blank" => r"\t\p{Zs}",
            "cntrl" => r"\p{Cc}",
            "graph" => r"[^\p{White_Space}\p{Cc}\p{Cs}\p{Cn}]",
            "print" => r"[^\p{White_Space}\p{Cc}\p{Cs}\p{Cn}]\p{Zs}",
            "word" => r"\w",
            _ => return None,
        };
        self.i = end + 2;
        Some(if negated { format!("[^{members}]") } else { members.to_owned() })
    }

    fn open_group(&mut self) {
        self.i += 1;
        let saved = self.flags;
        if self.peek(0) != Some('?') {
            self.out.push('(');
            self.levels.push(Level { saved, kind: Group::Plain, alt_open: false, cur_open: false });
            return;
        }
        match (self.peek(1), self.peek(2)) {
            (Some('#'), _) => {
                // A comment.
                while let Some(c) = self.peek(0) {
                    self.i += 1;
                    if c == ')' {
                        break;
                    }
                }
            }
            (Some('='), _) | (Some('!'), _) => {
                self.i += 2;
                self.out.push_str(&format!("(?{}", self.chars[self.i - 1]));
                self.levels.push(Level { saved, kind: Group::LookAhead, alt_open: false, cur_open: false });
                self.traits.looks_ahead = true;
            }
            (Some('<'), Some('=')) | (Some('<'), Some('!')) => {
                self.i += 3;
                self.out.push_str(&format!("(?<{}", self.chars[self.i - 1]));
                self.levels.push(Level { saved, kind: Group::LookBehind, alt_open: false, cur_open: false });
                self.traits.looks_behind = true;
            }
            (Some(':'), _) | (Some('>'), _) => {
                self.i += 2;
                self.out.push_str(&format!("(?{}", self.chars[self.i - 1]));
                self.levels.push(Level { saved, kind: Group::Plain, alt_open: false, cur_open: false });
            }
            (Some('<'), _) | (Some('P'), Some('<')) => {
                // A named group: copy its name.
                self.out.push('(');
                while let Some(c) = self.peek(0) {
                    self.i += 1;
                    self.out.push(c);
                    if c == '>' {
                        break;
                    }
                }
                self.levels.push(Level { saved, kind: Group::Plain, alt_open: false, cur_open: false });
            }
            _ => self.flag_group(saved),
        }
    }

    /// `(?flags)` or `(?flags:`, `self.i` at the `?`: the flags ICU and
    /// Rust share are handed on, the line flags followed here.
    fn flag_group(&mut self, saved: Flags) {
        self.i += 1;
        let mut on = true;
        let (mut keep_on, mut keep_off) = (String::new(), String::new());
        loop {
            let Some(c) = self.peek(0) else { return };
            self.i += 1;
            match c {
                '-' => on = false,
                'm' => self.flags.multi = on,
                's' => self.flags.dot_all = on,
                'w' => {}
                'x' => {
                    self.flags.extended = on;
                    if on { keep_on.push(c) } else { keep_off.push(c) }
                }
                ':' | ')' => {
                    let mut flags = keep_on.clone();
                    if !keep_off.is_empty() {
                        flags.push('-');
                        flags.push_str(&keep_off);
                    }
                    if c == ':' {
                        self.out.push_str(&format!("(?{flags}:"));
                        self.levels.push(Level { saved, kind: Group::Plain, alt_open: false, cur_open: false });
                    } else if !flags.is_empty() {
                        self.out.push_str(&format!("(?{flags})"));
                    }
                    return;
                }
                _ => {
                    if on {
                        keep_on.push(c)
                    } else {
                        keep_off.push(c)
                    }
                }
            }
        }
    }

    fn close_group(&mut self) {
        self.i += 1;
        self.out.push(')');
        if self.levels.len() == 1 {
            // Unbalanced: the engine reports it.
            return;
        }
        let inner = self.levels.pop().expect("a group");
        self.flags = inner.saved;
        let open = inner.alt_open || inner.cur_open;
        match inner.kind {
            Group::Plain => {
                self.level().cur_open = open;
                self.started = true;
            }
            Group::LookAhead => self.peeks(),
            Group::LookBehind => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traits() {
        let t = |p: &str| translate(p, 0).traits;
        assert!(t("b+").open_end && !t("a+b").open_end && !t("a|ab").open_end && t("ba?").open_end);
        assert!(!t("a+?").open_end && t("a{1,}").open_end && !t("a{2}").open_end && t("a\\b").open_end);
        assert!(t("b$").open_end && t("b$").anchors_end && t("(a|b+)").open_end);
        assert!(t("^ab").anchored && t("\\Aab").anchored && !t("a^").anchored && !t("^a|b").anchored);
        assert!(!translate("^a", ANCHORS_MATCH_LINES).traits.anchored);
        assert!(t("(?<=a)b").looks_behind && t("b(?=c)").looks_ahead && t("\\bx").word_bounds);
    }

    #[test]
    fn rewrites() {
        let p = |p: &str| translate(p, 0).pattern;
        assert_eq!(p("(?m)a"), "a");
        assert_eq!(p("(?mi)a"), "(?i)a");
        assert_eq!(p("(?-s:a)"), "(?:a)");
        assert_eq!(p("(?#note)a"), "a");
        assert_eq!(p("[[:alpha:]]"), r"[\p{Alphabetic}]");
        assert_eq!(p("[^[:^digit:]x]"), r"[^[^\p{Nd}]x]");
        assert_eq!(p("\\0141"), r"\x{61}");
        assert_eq!(p("\\cA"), r"\x{1}");
        assert_eq!(p("a{2,"), r"a\{2,");
        assert_eq!(p("\\p{L}+"), r"\p{L}+");
        assert_eq!(p("(?<name>a)"), "(?<name>a)");
    }
}
