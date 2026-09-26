//! Unicode TR35 date patterns: formatting, parsing, the en_US patterns
//! behind `NSDateFormatter`'s styles, and skeleton templates.
//!
//! Symbols are English (overridable per formatter). The style patterns and
//! the template results are the ones macOS produces for `en_US`, pinned by
//! `conformance/tests/dates.rs`; other locales get them too.
//!
//! Parsing follows ICU's rules where macOS shows them: numbers take as many
//! digits as they have unless the next field is a number too (then
//! exactly the pattern's width), text fields match case-insensitively,
//! weekday names are read and ignored, two-digit years fall within the
//! century from 1950, missing fields come from 2000-01-01 00:00:00 in the
//! formatter's zone, and out-of-range values fail unless lenient.

use crate::time_zone::{Zone, long_gmt, short_gmt};

/// The symbols a formatter uses.
#[derive(Clone, Debug)]
pub(crate) struct Symbols {
    pub(crate) months: Vec<String>,
    pub(crate) short_months: Vec<String>,
    pub(crate) weekdays: Vec<String>,
    pub(crate) short_weekdays: Vec<String>,
    pub(crate) am: String,
    pub(crate) pm: String,
    pub(crate) eras: Vec<String>,
    pub(crate) long_eras: Vec<String>,
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

impl Default for Symbols {
    fn default() -> Self {
        Symbols {
            months: strings(&[
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ]),
            short_months: strings(&[
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ]),
            weekdays: strings(&["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]),
            short_weekdays: strings(&["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]),
            am: "AM".into(),
            pm: "PM".into(),
            eras: strings(&["BC", "AD"]),
            long_eras: strings(&["Before Christ", "Anno Domini"]),
        }
    }
}

const NARROW_MONTHS: [&str; 12] = ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"];
const NARROW_WEEKDAYS: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];
const SHORTEST_WEEKDAYS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
const QUARTERS: [&str; 4] = ["1st quarter", "2nd quarter", "3rd quarter", "4th quarter"];

/// A pattern, split into literal text and fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    Literal(String),
    Field(char, usize),
}

/// Split a pattern: letters are fields, repeated letters one field,
/// `'...'` quoted text (`''` a quote), anything else literal.
pub(crate) fn tokenize(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let mut literal = String::new();
    let mut i = 0;
    let flush = |literal: &mut String, out: &mut Vec<Token>| {
        if !literal.is_empty() {
            out.push(Token::Literal(std::mem::take(literal)));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                literal.push('\'');
                i += 2;
                continue;
            }
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        literal.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                literal.push(chars[i]);
                i += 1;
            }
        } else if c.is_ascii_alphabetic() {
            flush(&mut literal, &mut out);
            let start = i;
            while i < chars.len() && chars[i] == c {
                i += 1;
            }
            out.push(Token::Field(c, i - start));
        } else {
            literal.push(c);
            i += 1;
        }
    }
    flush(&mut literal, &mut out);
    out
}

/// Days since 1970-01-01 of a date in the calendar macOS uses: Julian
/// before 1582-10-15, Gregorian from then on.
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    if (year, month, day) < (1582, 10, 15) {
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(4);
        let yoe = y - era * 4;
        let m = i64::from(month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
        return era * 1461 + yoe * 365 + doy - 719_470;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn is_leap(year: i64) -> bool {
    if year < 1582 {
        return year.rem_euclid(4) == 0;
    }
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

pub(crate) fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// A moment's local calendar fields.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Local {
    pub(crate) year: i64,
    pub(crate) month: u32,
    pub(crate) day: u32,
    pub(crate) hour: u32,
    pub(crate) minute: u32,
    pub(crate) second: u32,
    pub(crate) nanos: u32,
    /// 0 for Sunday.
    pub(crate) weekday: u32,
    pub(crate) day_of_year: u32,
    pub(crate) days: i64,
}

/// Local fields of a moment (seconds since the reference date) at an
/// offset.
pub(crate) fn local(seconds: f64, offset: i32) -> Local {
    let unix = seconds + crate::date::UNIX_TO_REFERENCE + f64::from(offset);
    let whole = unix.floor();
    let nanos = (((unix - whole) * 1e9).round() as u32).min(999_999_999);
    let whole = whole as i64;
    let days = whole.div_euclid(86_400);
    let rest = whole.rem_euclid(86_400) as u32;
    let (year, month, day) = crate::date::civil_from_days(days);
    Local {
        year,
        month,
        day,
        hour: rest / 3600,
        minute: rest / 60 % 60,
        second: rest % 60,
        nanos,
        weekday: (days + 4).rem_euclid(7) as u32,
        day_of_year: (days - days_from_civil(year, 1, 1)) as u32 + 1,
        days,
    }
}

/// Weeks as en_US counts them: starting Sunday, the week holding January
/// 1st is the first. Returns the week-year and week.
fn week_of_year(l: &Local) -> (i64, u32) {
    let jan1_weekday = (days_from_civil(l.year, 1, 1) + 4).rem_euclid(7) as u32;
    let week = (l.day_of_year - 1 + jan1_weekday) / 7 + 1;
    // The last days of December share a week with the next January 1st.
    let next_jan1 = days_from_civil(l.year + 1, 1, 1);
    let week_start = l.days - i64::from(l.weekday);
    if next_jan1 - week_start < 7 { (l.year + 1, 1) } else { (l.year, week) }
}

fn pad(out: &mut String, value: i64, width: usize) {
    let digits = value.unsigned_abs().to_string();
    if value < 0 {
        out.push('-');
    }
    for _ in digits.len()..width {
        out.push('0');
    }
    out.push_str(&digits);
}

fn offset_text(offset: i32, colon: bool, minutes: Minutes, z_for_zero: bool) -> String {
    if offset == 0 && z_for_zero {
        return "Z".into();
    }
    let sign = if offset < 0 { '-' } else { '+' };
    let a = offset.unsigned_abs();
    let (h, m, s) = (a / 3600, a / 60 % 60, a % 60);
    let mut out = format!("{sign}{h:02}");
    let show_minutes = matches!(minutes, Minutes::Always | Minutes::WithSeconds) || m != 0;
    if show_minutes {
        if colon {
            out.push(':');
        }
        out.push_str(&format!("{m:02}"));
    }
    if matches!(minutes, Minutes::WithSeconds) && s != 0 {
        if colon {
            out.push(':');
        }
        out.push_str(&format!("{s:02}"));
    }
    out
}

#[derive(Clone, Copy)]
enum Minutes {
    IfAny,
    Always,
    WithSeconds,
}

/// Format a moment with a pattern.
pub(crate) fn format(tokens: &[Token], seconds: f64, zone: &Zone, symbols: &Symbols) -> String {
    let moment = zone.at(seconds);
    let l = local(seconds, moment.offset);
    let mut out = String::new();
    for token in tokens {
        let (letter, n) = match token {
            Token::Literal(text) => {
                out.push_str(text);
                continue;
            }
            Token::Field(letter, n) => (*letter, *n),
        };
        let era_year = if l.year <= 0 { 1 - l.year } else { l.year };
        match letter {
            'G' => out.push_str(match n {
                4 => &symbols.long_eras[usize::from(l.year > 0)],
                5 => ["B", "A"][usize::from(l.year > 0)],
                _ => &symbols.eras[usize::from(l.year > 0)],
            }),
            'y' if n == 2 => pad(&mut out, era_year % 100, 2),
            'y' => pad(&mut out, era_year, n),
            'Y' => {
                let (week_year, _) = week_of_year(&l);
                if n == 2 { pad(&mut out, week_year.rem_euclid(100), 2) } else { pad(&mut out, week_year, n) }
            }
            'u' | 'U' | 'r' => pad(&mut out, l.year, n),
            'Q' | 'q' => {
                let q = (l.month - 1) / 3;
                match n {
                    1 | 2 => pad(&mut out, i64::from(q + 1), n),
                    3 => out.push_str(&format!("Q{}", q + 1)),
                    4 => out.push_str(QUARTERS[q as usize]),
                    _ => out.push_str(&(q + 1).to_string()),
                }
            }
            'M' | 'L' => match n {
                1 | 2 => pad(&mut out, i64::from(l.month), n),
                3 => out.push_str(&symbols.short_months[l.month as usize - 1]),
                4 => out.push_str(&symbols.months[l.month as usize - 1]),
                _ => out.push_str(NARROW_MONTHS[l.month as usize - 1]),
            },
            'w' => pad(&mut out, i64::from(week_of_year(&l).1), n),
            'W' => {
                let first = (l.days - i64::from(l.day) + 1 + 4).rem_euclid(7) as u32;
                pad(&mut out, i64::from((l.day - 1 + first) / 7 + 1), n);
            }
            'd' => pad(&mut out, i64::from(l.day), n),
            'D' => pad(&mut out, i64::from(l.day_of_year), n),
            'F' => pad(&mut out, i64::from((l.day - 1) / 7 + 1), n),
            'g' => pad(&mut out, l.days + 2_440_588, n),
            'E' => weekday_text(&mut out, &l, n.max(3), symbols),
            'e' | 'c' if n <= 2 => {
                let local = i64::from(l.weekday + 1);
                if letter == 'e' { pad(&mut out, local, n) } else { pad(&mut out, local, 1) }
            }
            'e' | 'c' => weekday_text(&mut out, &l, n, symbols),
            'a' | 'b' | 'B' => {
                let pm = l.hour >= 12;
                if n == 5 {
                    out.push_str(if pm { "p" } else { "a" });
                } else {
                    out.push_str(if pm { &symbols.pm } else { &symbols.am });
                }
            }
            'h' => pad(&mut out, i64::from(if l.hour.is_multiple_of(12) { 12 } else { l.hour % 12 }), n),
            'H' => pad(&mut out, i64::from(l.hour), n),
            'K' => pad(&mut out, i64::from(l.hour % 12), n),
            'k' => pad(&mut out, i64::from(if l.hour == 0 { 24 } else { l.hour }), n),
            'm' => pad(&mut out, i64::from(l.minute), n),
            's' => pad(&mut out, i64::from(l.second), n),
            'S' => {
                let digits = format!("{:09}", l.nanos);
                if n <= 9 {
                    out.push_str(&digits[..n]);
                } else {
                    out.push_str(&digits);
                    out.extend(std::iter::repeat_n('0', n - 9));
                }
            }
            'A' => pad(
                &mut out,
                i64::from(l.hour * 3_600_000 + l.minute * 60_000 + l.second * 1000 + l.nanos / 1_000_000),
                n,
            ),
            'z' if n < 4 => out.push_str(&zone.short_specific(seconds)),
            'z' => out.push_str(&zone.long_name(seconds)),
            'Z' => match n {
                1..=3 => out.push_str(&offset_text(moment.offset, false, Minutes::Always, false)),
                4 => out.push_str(&long_gmt(moment.offset)),
                _ => out.push_str(&offset_text(moment.offset, true, Minutes::Always, true)),
            },
            'O' if n < 4 => {
                out.push_str(&if moment.offset == 0 { "GMT+0".to_string() } else { short_gmt(moment.offset) })
            }
            'O' => out.push_str(&long_gmt(moment.offset)),
            'v' if n < 4 => out.push_str(&zone.generic_short(seconds)),
            'v' => out.push_str(&zone.generic_long(seconds)),
            'V' => match n {
                1 => out.push_str(if zone.name == "GMT" { "gmt" } else { "unk" }),
                2 if zone.fixed.is_some() && zone.name != "GMT" => out.push_str(&long_gmt(moment.offset)),
                2 => out.push_str(&zone.name),
                3 => out.push_str(&zone.exemplar_city()),
                _ => out.push_str(&zone.generic_location(seconds)),
            },
            'X' | 'x' => {
                let z = letter == 'X';
                out.push_str(&match n {
                    1 => offset_text(moment.offset, false, Minutes::IfAny, z),
                    2 => offset_text(moment.offset, false, Minutes::Always, z),
                    3 => offset_text(moment.offset, true, Minutes::Always, z),
                    4 => offset_text(moment.offset, false, Minutes::WithSeconds, z),
                    _ => offset_text(moment.offset, true, Minutes::WithSeconds, z),
                });
            }
            other => out.extend(std::iter::repeat_n(other, n)),
        }
    }
    out
}

fn weekday_text(out: &mut String, l: &Local, n: usize, symbols: &Symbols) {
    let w = l.weekday as usize;
    match n {
        4 => out.push_str(&symbols.weekdays[w]),
        5 => out.push_str(NARROW_WEEKDAYS[w]),
        6 => out.push_str(SHORTEST_WEEKDAYS[w]),
        _ => out.push_str(&symbols.short_weekdays[w]),
    }
}

fn is_numeric(token: &Token) -> bool {
    match token {
        Token::Field(c, n) => match c {
            'y' | 'Y' | 'u' | 'U' | 'r' | 'd' | 'D' | 'F' | 'g' | 'h' | 'H' | 'K' | 'k' | 'm' | 's' | 'S' | 'A'
            | 'w' | 'W' => true,
            'M' | 'L' | 'Q' | 'q' | 'e' | 'c' => *n <= 2,
            _ => false,
        },
        Token::Literal(_) => false,
    }
}

/// What a parse found.
#[derive(Default, Debug)]
struct Found {
    /// The value, the pattern's width and the digits read.
    year: Option<(i64, usize, usize)>,
    bc: bool,
    month: Option<u32>,
    day: Option<u32>,
    day_of_year: Option<u32>,
    hour: Option<(u32, char)>,
    pm: Option<bool>,
    minute: Option<u32>,
    second: Option<u32>,
    fraction: Option<f64>,
    offset: Option<i32>,
    zone: Option<Zone>,
}

struct Input<'a> {
    chars: Vec<char>,
    at: usize,
    _text: &'a str,
}

impl Input<'_> {
    fn rest(&self) -> String {
        self.chars[self.at..].iter().collect()
    }

    fn skip_space(&mut self) {
        while self.chars.get(self.at).is_some_and(|c| c.is_whitespace()) {
            self.at += 1;
        }
    }

    fn digits(&mut self, max: usize) -> Option<(i64, usize)> {
        let start = self.at;
        while self.at < self.chars.len() && self.at - start < max && self.chars[self.at].is_ascii_digit() {
            self.at += 1;
        }
        let text: String = self.chars[start..self.at].iter().collect();
        (!text.is_empty()).then(|| text.parse().ok().map(|v| (v, text.len()))).flatten()
    }

    /// Match one of some names, longest first, ignoring case.
    fn names(&mut self, names: &[&str]) -> Option<usize> {
        let rest = self.rest().to_lowercase();
        let mut best: Option<(usize, usize)> = None;
        for (i, name) in names.iter().enumerate() {
            let lower = name.to_lowercase();
            if !lower.is_empty() && rest.starts_with(&lower) && best.is_none_or(|(_, len)| lower.chars().count() > len)
            {
                best = Some((i, lower.chars().count()));
            }
        }
        let (i, len) = best?;
        self.at += len;
        Some(i)
    }

    fn literal(&mut self, text: &str) -> Option<()> {
        for c in text.chars() {
            if c.is_whitespace() {
                self.skip_space();
                continue;
            }
            let got = *self.chars.get(self.at)?;
            if got.to_lowercase().ne(c.to_lowercase()) {
                return None;
            }
            self.at += 1;
        }
        Some(())
    }

    /// `+05:30`, `-0500`, `Z`.
    fn iso_offset(&mut self) -> Option<i32> {
        if matches!(self.chars.get(self.at), Some('Z' | 'z')) {
            self.at += 1;
            return Some(0);
        }
        let sign = match self.chars.get(self.at)? {
            '+' => 1,
            '-' => -1,
            _ => return None,
        };
        self.at += 1;
        let (h, len) = self.digits(2)?;
        let mut minutes = 0;
        if len == 2 {
            if self.chars.get(self.at) == Some(&':') {
                self.at += 1;
            }
            if let Some((m, _)) = self.digits(2) {
                minutes = m;
            }
        }
        Some(sign * (h as i32 * 3600 + minutes as i32 * 60))
    }

    /// `GMT`, `GMT+5:30`, `UTC-04:00`.
    fn gmt_offset(&mut self) -> Option<i32> {
        let start = self.at;
        self.names(&["GMT", "UTC", "UT"])?;
        let sign = match self.chars.get(self.at) {
            Some('+') => 1,
            Some('-') => -1,
            _ => return Some(0),
        };
        self.at += 1;
        let Some((h, len)) = self.digits(2) else {
            self.at = start;
            return None;
        };
        let mut minutes = 0;
        if self.chars.get(self.at) == Some(&':') {
            self.at += 1;
            minutes = self.digits(2).map_or(0, |(m, _)| m);
        } else if len == 2 {
            minutes = self.digits(2).map_or(0, |(m, _)| m);
        }
        Some(sign * (h as i32 * 3600 + minutes as i32 * 60))
    }
}

/// Offsets for zone names and abbreviations a parse may meet.
const ZONE_NAMES: &[(&str, i32)] = &[
    ("Eastern Standard Time", -18000),
    ("Eastern Daylight Time", -14400),
    ("Central Standard Time", -21600),
    ("Central Daylight Time", -18000),
    ("Mountain Standard Time", -25200),
    ("Mountain Daylight Time", -21600),
    ("Pacific Standard Time", -28800),
    ("Pacific Daylight Time", -25200),
    ("Greenwich Mean Time", 0),
    ("Coordinated Universal Time", 0),
    ("Central European Standard Time", 3600),
    ("Central European Summer Time", 7200),
    ("India Standard Time", 19800),
    ("Japan Standard Time", 32400),
    ("EST", -18000),
    ("EDT", -14400),
    ("CST", -21600),
    ("CDT", -18000),
    ("MST", -25200),
    ("MDT", -21600),
    ("PST", -28800),
    ("PDT", -25200),
    ("AKST", -32400),
    ("AKDT", -28800),
    ("HST", -36000),
    ("CET", 3600),
    ("CEST", 7200),
    ("BST", 3600),
];

/// Parse text with a pattern, in a zone; seconds since the reference
/// date.
pub(crate) fn parse(
    tokens: &[Token],
    text: &str,
    zone: &Zone,
    symbols: &Symbols,
    lenient: bool,
    two_digit_start: i64,
) -> Option<f64> {
    let trimmed = text.trim();
    let mut input = Input { chars: trimmed.chars().collect(), at: 0, _text: trimmed };
    let mut found = Found::default();
    for (i, token) in tokens.iter().enumerate() {
        let (letter, n) = match token {
            Token::Literal(literal) => {
                input.literal(literal)?;
                continue;
            }
            Token::Field(letter, n) => (*letter, *n),
        };
        let abutting = tokens.get(i + 1).is_some_and(is_numeric);
        if is_numeric(token) {
            let max = if abutting { n.max(1) } else { 10 };
            let (value, len) = input.digits(max)?;
            match letter {
                'y' | 'Y' | 'u' | 'U' | 'r' => found.year = Some((value, n, len)),
                'M' | 'L' => found.month = Some(value as u32),
                'd' => found.day = Some(value as u32),
                'D' => found.day_of_year = Some(value as u32),
                'h' | 'H' | 'K' | 'k' => found.hour = Some((value as u32, letter)),
                'm' => found.minute = Some(value as u32),
                's' => found.second = Some(value as u32),
                'S' => found.fraction = Some(value as f64 / 10f64.powi(len as i32)),
                _ => {}
            }
            continue;
        }
        match letter {
            'M' | 'L' => {
                let names: Vec<&str> = symbols.months.iter().chain(&symbols.short_months).map(String::as_str).collect();
                found.month = Some((input.names(&names)? % 12) as u32 + 1);
            }
            'E' | 'e' | 'c' => {
                let names: Vec<&str> =
                    symbols.weekdays.iter().chain(&symbols.short_weekdays).map(String::as_str).collect();
                input.names(&names)?;
            }
            'G' => {
                let names: Vec<&str> = symbols.long_eras.iter().chain(&symbols.eras).map(String::as_str).collect();
                found.bc = input.names(&names)? % 2 == 0;
            }
            'a' | 'b' | 'B' => found.pm = Some(input.names(&[&symbols.am, &symbols.pm, "a", "p"])? % 2 == 1),
            'Q' | 'q' => {
                input.names(&["1st quarter", "2nd quarter", "3rd quarter", "4th quarter", "Q1", "Q2", "Q3", "Q4"])?;
            }
            'z' | 'Z' | 'O' | 'v' | 'V' | 'X' | 'x' => {
                let start = input.at;
                let offset = input.iso_offset().or_else(|| {
                    input.at = start;
                    input.gmt_offset()
                });
                let offset = offset.or_else(|| {
                    input.at = start;
                    let names: Vec<&str> = ZONE_NAMES.iter().map(|(n, _)| *n).collect();
                    input.names(&names).map(|i| ZONE_NAMES[i].1)
                });
                match offset {
                    Some(offset) => found.offset = Some(offset),
                    None => {
                        // A zone identifier.
                        input.at = start;
                        let id: String = input.chars[input.at..]
                            .iter()
                            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
                            .collect();
                        let named = Zone::named(&id)?;
                        input.at += id.chars().count();
                        found.zone = Some(named);
                    }
                }
            }
            _ => return None,
        }
    }
    input.skip_space();
    if input.at < input.chars.len() {
        return None;
    }
    resolve(found, zone, lenient, two_digit_start)
}

fn resolve(found: Found, zone: &Zone, lenient: bool, two_digit_start: i64) -> Option<f64> {
    let mut year = match found.year {
        Some((y, 2, len)) if len <= 2 => {
            let base = two_digit_start - two_digit_start.rem_euclid(100);
            let candidate = base + y;
            if candidate < two_digit_start { candidate + 100 } else { candidate }
        }
        Some((y, _, _)) => y,
        None => 2000,
    };
    if found.bc {
        year = 1 - year;
    }
    let mut hour = match found.hour {
        Some((h, 'h')) => {
            if !lenient && !(1..=12).contains(&h) {
                return None;
            }
            h % 12
        }
        Some((h, 'K')) => h,
        Some((h, 'k')) => {
            if !lenient && !(1..=24).contains(&h) {
                return None;
            }
            h % 24
        }
        Some((h, _)) => h,
        None => 0,
    };
    if found.pm == Some(true) && hour < 12 {
        hour += 12;
    }
    let (minute, second) = (found.minute.unwrap_or(0), found.second.unwrap_or(0));
    let (month, day) = match (found.month, found.day, found.day_of_year) {
        (None, None, Some(doy)) => {
            let days = days_from_civil(year, 1, 1) + i64::from(doy) - 1;
            let (_, m, d) = crate::date::civil_from_days(days);
            (m, d)
        }
        (m, d, _) => (m.unwrap_or(1), d.unwrap_or(1)),
    };
    if !lenient
        && (!(1..=12).contains(&month)
            || day < 1
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 59)
    {
        return None;
    }
    // Lenient values roll over: month 13 is next January.
    let month0 = i64::from(month) - 1;
    let year = year + month0.div_euclid(12);
    let month = month0.rem_euclid(12) as u32 + 1;
    let days = days_from_civil(year, month, 1) + i64::from(day) - 1;
    let local_seconds = days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
    let fraction = found.fraction.unwrap_or(0.0);
    let unix = match (found.offset, &found.zone) {
        (Some(offset), _) => local_seconds - i64::from(offset),
        (None, Some(named)) => offset_of_local(named, local_seconds),
        (None, None) => offset_of_local(zone, local_seconds),
    };
    Some(unix as f64 + fraction - crate::date::UNIX_TO_REFERENCE)
}

/// The Unix time of a local time in a zone, taking the earlier of two
/// meanings at a fall-back and moving forward over a spring-forward gap.
fn offset_of_local(zone: &Zone, local_seconds: i64) -> i64 {
    if let Some(offset) = zone.fixed {
        return local_seconds - i64::from(offset);
    }
    let days = local_seconds.div_euclid(86_400);
    let rest = local_seconds.rem_euclid(86_400);
    let (y, m, d) = crate::date::civil_from_days(days);
    let dt = jiff::civil::DateTime::new(
        y as i16,
        m as i8,
        d as i8,
        (rest / 3600) as i8,
        (rest / 60 % 60) as i8,
        (rest % 60) as i8,
        0,
    );
    match dt.ok().and_then(|dt| zone.tz.to_ambiguous_timestamp(dt).compatible().ok()) {
        Some(ts) => ts.as_second(),
        None => local_seconds,
    }
}

/// The en_US pattern for a date style and a time style (0 none, 1 short,
/// 2 medium, 3 long, 4 full).
pub(crate) fn style_pattern(date: usize, time: usize) -> String {
    let date_part = match date {
        1 => "M/d/yy",
        2 => "MMM d, y",
        3 => "MMMM d, y",
        4 => "EEEE, MMMM d, y",
        _ => "",
    };
    let time_part = match time {
        1 => "h:mm\u{202f}a",
        2 => "h:mm:ss\u{202f}a",
        3 => "h:mm:ss\u{202f}a z",
        4 => "h:mm:ss\u{202f}a zzzz",
        _ => "",
    };
    match (date_part.is_empty(), time_part.is_empty()) {
        (true, _) => time_part.to_string(),
        (_, true) => date_part.to_string(),
        _ if date == 1 => format!("{date_part}, {time_part}"),
        _ => format!("{date_part} 'at' {time_part}"),
    }
}

/// The en_US pattern for a skeleton such as `yMMMd` or `jmm`.
pub(crate) fn pattern_from_template(template: &str) -> String {
    let mut counts: Vec<(char, usize)> = Vec::new();
    for c in template.chars().filter(|c| c.is_ascii_alphabetic()) {
        match counts.iter_mut().find(|(l, _)| *l == c) {
            Some((_, n)) => *n += 1,
            None => counts.push((c, 1)),
        }
    }
    let count = |letters: &str| counts.iter().find(|(l, _)| letters.contains(*l)).map(|&(l, n)| (l, n));
    let rep = |c: char, n: usize| std::iter::repeat_n(c, n).collect::<String>();

    // The date half.
    let year = count("yYu").map(|(_, n)| rep('y', n));
    let month = count("ML");
    let day = count("d").map(|(_, n)| rep('d', n.min(2)));
    let weekday = count("Eec").map(|(_, n)| if n >= 4 { "EEEE".to_string() } else { "EEE".to_string() });
    let quarter = count("Qq").map(|(_, n)| rep('Q', n.clamp(1, 4)));
    let date = match (&year, month, &day, &weekday, &quarter) {
        (Some(y), None, None, None, Some(q)) => format!("{q} {y}"),
        (None, None, None, None, Some(q)) => q.clone(),
        (None, Some((_, n)), None, None, None) if n >= 3 => rep('L', n.min(4)),
        (None, Some((_, n)), None, None, None) => rep('L', n),
        (None, None, None, Some(w), None) => w.replace('E', "c"),
        (Some(y), None, None, None, None) => y.clone(),
        (None, None, Some(d), None, None) => d.clone(),
        (None, None, Some(d), Some(w), None) => format!("{w} {d}"),
        (y, Some((_, n)), d, w, _) if n >= 3 => {
            let m = rep('M', n.min(4));
            let mut text = match (d, y) {
                (Some(d), Some(y)) => format!("{m} {d}, {y}"),
                (Some(d), None) => format!("{m} {d}"),
                (None, Some(y)) => format!("{m} {y}"),
                (None, None) => m.clone(),
            };
            if let Some(w) = w {
                text = format!("{w}, {text}");
            }
            text
        }
        (y, Some((_, n)), d, w, _) => {
            let m = rep('M', n.min(2));
            let mut text = match (d, y) {
                (Some(d), Some(y)) => format!("{m}/{d}/{y}"),
                (Some(d), None) => format!("{m}/{d}"),
                (None, Some(y)) => format!("{m}/{y}"),
                (None, None) => m.clone(),
            };
            if let Some(w) = w {
                text = format!("{w}, {text}");
            }
            text
        }
        _ => String::new(),
    };

    // The time half.
    let hour = count("jhHkK");
    let minute = count("m").map(|_| "mm");
    let second = count("s").map(|_| "ss");
    let zone = count("zvVZO").map(|(l, n)| rep(l, if n >= 4 { 4 } else { 1 }));
    let mut time = String::new();
    if let Some((letter, n)) = hour {
        let twelve = matches!(letter, 'j' | 'h' | 'K');
        let hour_field = match letter {
            'j' | 'h' => "h",
            'K' => "K",
            'k' => "kk",
            _ => "HH",
        };
        time.push_str(hour_field);
        if let Some(m) = minute {
            time.push(':');
            time.push_str(m);
            if let Some(s) = second {
                time.push(':');
                time.push_str(s);
            }
        }
        if twelve {
            time.push('\u{202f}');
            time.push_str(if letter == 'j' && n == 2 && minute.is_none() { "aaaa" } else { "a" });
        }
    } else if let Some(m) = minute {
        time.push_str(m);
        if let Some(s) = second {
            time.push(':');
            time.push_str(s);
        }
    } else if let Some(s) = second {
        time.push_str(s);
    }
    if let Some(z) = zone {
        if !time.is_empty() {
            time.push(' ');
        }
        time.push_str(&z);
    }
    match (date.is_empty(), time.is_empty()) {
        (true, _) => time,
        (_, true) => date,
        _ => format!("{date}, {time}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gmt() -> Zone {
        Zone::fixed(0)
    }

    fn f(pattern: &str, seconds: f64, zone: &Zone) -> String {
        format(&tokenize(pattern), seconds, zone, &Symbols::default())
    }

    fn p(pattern: &str, text: &str) -> Option<f64> {
        parse(&tokenize(pattern), text, &gmt(), &Symbols::default(), false, 1950)
    }

    const MOMENT: f64 = 780_000_000.5;

    #[test]
    fn formatting() {
        let z = gmt();
        assert_eq!(f("yyyy-MM-dd'T'HH:mm:ssZZZZZ", MOMENT, &z), "2025-09-19T18:40:00Z");
        assert_eq!(f("yyyy-MM-dd'T'HH:mm:ss.SSSZ", MOMENT, &z), "2025-09-19T18:40:00.500+0000");
        assert_eq!(f("EEE, dd MMM yyyy HH:mm:ss zzz", MOMENT, &z), "Fri, 19 Sep 2025 18:40:00 GMT");
        assert_eq!(f("EEEE MMMM d y G h:mm a", MOMENT, &z), "Friday September 19 2025 AD 6:40 PM");
        assert_eq!(f("yy M/d H:m:s", MOMENT, &z), "25 9/19 18:40:0");
        assert_eq!(f("D w W e c Q QQQQ", MOMENT, &z), "262 38 3 6 6 3 3rd quarter");
        assert_eq!(f("''", MOMENT, &z), "'");
        assert_eq!(f("'it''s' h", MOMENT, &z), "it's 6");
        assert_eq!(f("hh:mm aaa K k", MOMENT, &z), "06:40 PM 6 18");
        assert_eq!(f("LLLL LLL", MOMENT, &z), "September Sep");
        assert_eq!(f("EEEEE MMMMM", MOMENT, &z), "F S");
        assert_eq!(f("yyyyy", MOMENT, &z), "02025");
        assert_eq!(f("S SS SSSS", MOMENT, &z), "5 50 5000");
        assert_eq!(f("G GGGG GGGGG", MOMENT, &z), "AD Anno Domini A");
        assert_eq!(f("q qq qqq qqqq", MOMENT, &z), "3 03 Q3 3rd quarter");
        assert_eq!(f("E EE EEE EEEE EEEEE EEEEEE", MOMENT, &z), "Fri Fri Fri Friday F Fr");
        assert_eq!(f("e ee eee eeee", MOMENT, &z), "6 06 Fri Friday");
        assert_eq!(f("c cc ccc cccc", MOMENT, &z), "6 6 Fri Friday");
        assert_eq!(f("a aa aaa aaaa aaaaa", MOMENT, &z), "PM PM PM PM p");
        assert_eq!(f("F g A", MOMENT, &z), "3 2460938 67200500");
        assert_eq!(
            f("x xx xxx X XXX ZZZZ O OOOO v vvvv V VV", MOMENT, &z),
            "+00 +0000 +00:00 Z Z GMT+00:00 GMT+0 GMT+00:00 GMT Greenwich Mean Time gmt GMT"
        );
        let india = Zone::fixed(19800);
        assert_eq!(
            f("z zzzz Z ZZZZ ZZZZZ O x X XXX", MOMENT, &india),
            "GMT+5:30 GMT+05:30 +0530 GMT+05:30 +05:30 GMT+5:30 +0530 +0530 +05:30"
        );
        let ny = Zone::named("America/New_York").unwrap();
        assert_eq!(
            f("z zzzz O v vvvv VV VVV VVVV X", MOMENT, &ny),
            "EDT Eastern Daylight Time GMT-4 ET Eastern Time America/New_York New York New York Time -04"
        );
        assert_eq!(f("y yyyy G u", -63_200_000_000.0, &z), "3 0003 BC -2");
    }

    #[test]
    fn parsing() {
        let date = |y, m, d| (days_from_civil(y, m, d) * 86_400) as f64 - crate::date::UNIX_TO_REFERENCE;
        assert_eq!(p("yyyy-MM-dd'T'HH:mm:ssZZZZZ", "2025-09-19T18:40:00Z"), Some(780_000_000.0));
        assert_eq!(p("yyyy-MM-dd'T'HH:mm:ss.SSSZ", "2025-09-19T18:40:00.500+0000"), Some(MOMENT));
        assert_eq!(p("EEE, dd MMM yyyy HH:mm:ss zzz", "Fri, 19 Sep 2025 18:40:00 GMT"), Some(780_000_000.0));
        assert_eq!(p("EEEE MMMM d y G h:mm a", "Friday September 19 2025 AD 6:40 PM"), Some(780_000_000.0));
        assert_eq!(p("yyyy-MM-dd", "2024-02-30"), None);
        assert_eq!(p("yyyy-MM-dd", "2024-2-3"), Some(date(2024, 2, 3)));
        assert_eq!(p("yyyy-MM-dd", " 2024-02-03 "), Some(date(2024, 2, 3)));
        assert_eq!(p("yyyy-MM-dd", "2024-02-03x"), None);
        assert_eq!(p("yyyy-MM-dd", "24-02-03"), Some(-62_385_465_600.0), "Julian before 1582");
        assert_eq!(p("yyyy-MM-dd", "2024-13-01"), None);
        let lenient =
            |pattern: &str, text: &str| parse(&tokenize(pattern), text, &gmt(), &Symbols::default(), true, 1950);
        assert_eq!(lenient("yyyy-MM-dd", "2024-02-30"), Some(date(2024, 3, 1)));
        assert_eq!(lenient("yyyy-MM-dd", "2024-13-01"), Some(date(2025, 1, 1)));
        assert_eq!(p("yy", "25"), Some(date(2025, 1, 1)));
        assert_eq!(p("yy", "49"), Some(date(2049, 1, 1)));
        assert_eq!(p("yy", "50"), Some(date(1950, 1, 1)));
        assert_eq!(p("MMM d", "sep 19"), Some(date(2000, 9, 19)));
        assert_eq!(p("h:mm a", "6:40 pm"), Some(date(2000, 1, 1) + 67_200.0));
        assert_eq!(p("h a", "12 am"), Some(date(2000, 1, 1)));
        assert_eq!(p("h a", "12 pm"), Some(date(2000, 1, 1) + 43_200.0));
        assert_eq!(p("HH:mm z", "10:00 EST"), Some(date(2000, 1, 1) + 54_000.0));
        assert_eq!(p("HH:mm z", "10:00 GMT+5:30"), Some(date(2000, 1, 1) + 16_200.0));
        assert_eq!(p("HH:mm Z", "10:00 -0500"), Some(date(2000, 1, 1) + 54_000.0));
        assert_eq!(p("HH:mm ZZZZZ", "10:00 -05:00"), Some(date(2000, 1, 1) + 54_000.0));
        assert_eq!(p("HH:mm zzzz", "10:00 Eastern Standard Time"), Some(date(2000, 1, 1) + 54_000.0));
        assert_eq!(p("EEE MMM d", "Mon Sep 19"), Some(date(2000, 9, 19)), "weekdays are ignored");
        assert_eq!(p("yyyyMMdd", "20250919"), Some(date(2025, 9, 19)));
        assert_eq!(p("HHmmss", "184000"), Some(date(2000, 1, 1) + 67_200.0));
        assert_eq!(p("d/M/y", "31/12/2025"), Some(date(2025, 12, 31)));
        assert_eq!(p("yyyy", "-5"), None);
        assert_eq!(p("H:mm", "24:00"), None);
        assert_eq!(p("H:mm", "7:60"), None);
        assert_eq!(p("LLLL LLL", "September Sep"), Some(date(2000, 9, 1)));
    }

    #[test]
    fn styles_and_templates() {
        assert_eq!(style_pattern(2, 1), "MMM d, y 'at' h:mm\u{202f}a");
        assert_eq!(style_pattern(1, 3), "M/d/yy, h:mm:ss\u{202f}a z");
        assert_eq!(style_pattern(4, 0), "EEEE, MMMM d, y");
        assert_eq!(style_pattern(0, 4), "h:mm:ss\u{202f}a zzzz");
        for (template, pattern) in [
            ("yMMMd", "MMM d, y"),
            ("jmm", "h:mm\u{202f}a"),
            ("MMMMdyyyy", "MMMM d, yyyy"),
            ("Hm", "HH:mm"),
            ("yMd", "M/d/y"),
            ("EEEEdMMM", "EEEE, MMM d"),
            ("yMMMMEEEEd", "EEEE, MMMM d, y"),
            ("jmmss", "h:mm:ss\u{202f}a"),
            ("Hmmss", "HH:mm:ss"),
            ("yM", "M/y"),
            ("yMMM", "MMM y"),
            ("MMMd", "MMM d"),
            ("Md", "M/d"),
            ("Ed", "EEE d"),
            ("yMEd", "EEE, M/d/y"),
            ("MMMMd", "MMMM d"),
            ("yQQQ", "QQQ y"),
            ("hmmz", "h:mm\u{202f}a z"),
            ("jmmzzzz", "h:mm\u{202f}a zzzz"),
            ("d", "d"),
            ("MMMM", "LLLL"),
            ("EEEE", "cccc"),
            ("y", "y"),
            ("yyMMdd", "MM/dd/yy"),
        ] {
            assert_eq!(pattern_from_template(template), pattern, "{template}");
        }
    }
}
