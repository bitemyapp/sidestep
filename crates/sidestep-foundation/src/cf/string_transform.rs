//! The `CFStringTransform` transforms Sidestep has: those Unicode's data
//! defines by rule, as macOS applies them (measured there, see
//! `conformance/tests/cf_strings.rs`):
//!
//! - stripping combining marks, and diacritics (the same here): canonical
//!   decomposition, nonspacing marks removed, then composition;
//! - XML hex: characters other than printable ASCII become `&#xHEX;`, and
//!   back;
//! - fullwidth to halfwidth: fullwidth ASCII, the ideographic space, the
//!   fullwidth signs and katakana (voiced ones as a letter and a mark), and
//!   back;
//! - hiragana to katakana, and back.
//!
//! The transliterations (Latin to and from other scripts, Mandarin to
//! Latin) and Unicode names need ICU's rule and name data, which Sidestep
//! doesn't carry: `CFStringTransform` returns false for them, as it does
//! for a transform it doesn't know.

use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use icu_properties::props::GeneralCategory;

/// Export constant strings under two symbol names, Foundation's and
/// CoreFoundation's, one object for both, as on macOS.
macro_rules! shared_strings {
    ($($object:ident: $ns:ident, $cf:ident = $value:literal;)*) => {$(
        static $object: crate::ConstantString =
            crate::ConstantString::new(&crate::CONSTANT_STRING_CLASS, crate::ConstStr::new(concat!($value, "\0")));
        #[unsafe(no_mangle)]
        pub static $ns: sidestep_runtime::ObjectRef = $object.object_ref();
        #[unsafe(no_mangle)]
        pub static $cf: sidestep_runtime::ObjectRef = $object.object_ref();
    )*};
}

shared_strings! {
    LATIN_KATAKANA: NSStringTransformLatinToKatakana, kCFStringTransformLatinKatakana = ")kCFStringTransformLatinKatakana";
    LATIN_HIRAGANA: NSStringTransformLatinToHiragana, kCFStringTransformLatinHiragana = ")kCFStringTransformLatinHiragana";
    LATIN_HANGUL: NSStringTransformLatinToHangul, kCFStringTransformLatinHangul = ")kCFStringTransformLatinHangul";
    LATIN_ARABIC: NSStringTransformLatinToArabic, kCFStringTransformLatinArabic = ")kCFStringTransformLatinArabic";
    LATIN_HEBREW: NSStringTransformLatinToHebrew, kCFStringTransformLatinHebrew = ")kCFStringTransformLatinHebrew";
    LATIN_THAI: NSStringTransformLatinToThai, kCFStringTransformLatinThai = ")kCFStringTransformLatinThai";
    LATIN_CYRILLIC: NSStringTransformLatinToCyrillic, kCFStringTransformLatinCyrillic = ")kCFStringTransformLatinCyrillic";
    LATIN_GREEK: NSStringTransformLatinToGreek, kCFStringTransformLatinGreek = ")kCFStringTransformLatinGreek";
    TO_LATIN: NSStringTransformToLatin, kCFStringTransformToLatin = ")kCFStringTransformToLatin";
    MANDARIN_LATIN: NSStringTransformMandarinToLatin, kCFStringTransformMandarinLatin = ")kCFStringTransformMandarinLatin";
    HIRAGANA_KATAKANA: NSStringTransformHiraganaToKatakana, kCFStringTransformHiraganaKatakana = ")kCFStringTransformHiraganaKatakana";
    FULLWIDTH_HALFWIDTH: NSStringTransformFullwidthToHalfwidth, kCFStringTransformFullwidthHalfwidth = ")kCFStringTransformFullwidthHalfwidth";
    TO_XML_HEX: NSStringTransformToXMLHex, kCFStringTransformToXMLHex = ")kCFStringTransformToXMLHex";
    TO_UNICODE_NAME: NSStringTransformToUnicodeName, kCFStringTransformToUnicodeName = ")kCFStringTransformToUnicodeName";
    STRIP_COMBINING_MARKS: NSStringTransformStripCombiningMarks, kCFStringTransformStripCombiningMarks = ")kCFStringTransformStripCombiningMarks";
    STRIP_DIACRITICS: NSStringTransformStripDiacritics, kCFStringTransformStripDiacritics = ")kCFStringTransformStripDiacritics";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transform {
    StripMarks,
    XmlHex,
    FullwidthHalfwidth,
    HiraganaKatakana,
}

impl Transform {
    /// The transform a name (a `kCFStringTransform…` constant's value or
    /// the ICU identifier it stands for) names, if Sidestep has it.
    pub(crate) fn named(name: &str) -> Option<Transform> {
        Some(match name {
            ")kCFStringTransformStripCombiningMarks"
            | ")kCFStringTransformStripDiacritics"
            | "NFD; [:Nonspacing Mark:] Remove; NFC" => Transform::StripMarks,
            ")kCFStringTransformToXMLHex" | "Any-Hex/XML" => Transform::XmlHex,
            ")kCFStringTransformFullwidthHalfwidth" | "Fullwidth-Halfwidth" => Transform::FullwidthHalfwidth,
            ")kCFStringTransformHiraganaKatakana" | "Hiragana-Katakana" => Transform::HiraganaKatakana,
            _ => return None,
        })
    }

    pub(crate) fn apply(self, text: &str, reverse: bool) -> String {
        match (self, reverse) {
            (Transform::StripMarks, false) => strip_marks(text),
            // Nothing brings marks back.
            (Transform::StripMarks, true) => text.to_string(),
            (Transform::XmlHex, false) => to_xml_hex(text),
            (Transform::XmlHex, true) => from_xml_hex(text),
            (Transform::FullwidthHalfwidth, false) => to_halfwidth(text),
            (Transform::FullwidthHalfwidth, true) => to_fullwidth(text),
            (Transform::HiraganaKatakana, false) => to_katakana(text),
            (Transform::HiraganaKatakana, true) => to_hiragana(text),
        }
    }
}

fn is_nonspacing_mark(c: char) -> bool {
    crate::string::fold::general_category(u32::from(c)) == GeneralCategory::NonspacingMark
}

fn strip_marks(text: &str) -> String {
    let decomposed: String =
        DecomposingNormalizerBorrowed::new_nfd().normalize(text).chars().filter(|&c| !is_nonspacing_mark(c)).collect();
    ComposingNormalizerBorrowed::new_nfc().normalize(&decomposed).into_owned()
}

fn to_xml_hex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if (' '..='~').contains(&c) {
            out.push(c);
        } else {
            out.push_str(&format!("&#x{:X};", u32::from(c)));
        }
    }
    out
}

/// `&#x` (a lowercase x), hex digits and `;` make a character; anything
/// else stays as it is.
fn from_xml_hex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("&#x") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 3..];
        let digits = after.bytes().take_while(u8::is_ascii_hexdigit).count();
        let decoded = (digits > 0 && after[digits..].starts_with(';'))
            .then(|| u32::from_str_radix(&after[..digits], 16).ok().and_then(char::from_u32))
            .flatten();
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[digits + 1..];
            }
            None => {
                out.push_str("&#x");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The halfwidth katakana block's characters and the fullwidth ones each
/// stands for (their compatibility decomposition).
fn halfwidth_katakana() -> impl Iterator<Item = (char, char)> {
    let nfkd = DecomposingNormalizerBorrowed::new_nfkd();
    ('\u{FF61}'..='\u{FF9F}').filter_map(move |half| {
        let mut full = nfkd.normalize_iter(std::iter::once(half));
        let first = full.next()?;
        full.next().is_none().then_some((half, first))
    })
}

/// The fullwidth signs and their ordinary forms.
const SIGNS: [(char, char); 7] = [
    ('\u{FFE0}', '\u{A2}'),
    ('\u{FFE1}', '\u{A3}'),
    ('\u{FFE2}', '\u{AC}'),
    ('\u{FFE3}', '\u{AF}'),
    ('\u{FFE4}', '\u{A6}'),
    ('\u{FFE5}', '\u{A5}'),
    ('\u{FFE6}', '\u{20A9}'),
];

fn to_halfwidth(text: &str) -> String {
    let table: std::collections::HashMap<char, char> = halfwidth_katakana().map(|(half, full)| (full, half)).collect();
    let nfd = DecomposingNormalizerBorrowed::new_nfd();
    let single = |c: char| match c {
        '\u{3000}' => ' ',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(u32::from(c) - 0xFEE0).unwrap_or(c),
        '\u{309B}' | '\u{3099}' => '\u{FF9E}',
        '\u{309C}' | '\u{309A}' => '\u{FF9F}',
        _ => match SIGNS.iter().find(|s| s.0 == c) {
            Some(&(_, plain)) => plain,
            None => table.get(&c).copied().unwrap_or(c),
        },
    };
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        // A voiced kana is a letter and a voicing mark, each of which has a
        // halfwidth form.
        let mut parts = nfd.normalize_iter(std::iter::once(c));
        match (parts.next(), parts.next(), parts.next()) {
            (Some(base), Some(mark @ ('\u{3099}' | '\u{309A}')), None) if table.contains_key(&base) => {
                out.push(single(base));
                out.push(single(mark));
            }
            _ => out.push(single(c)),
        }
    }
    out
}

fn to_fullwidth(text: &str) -> String {
    let table: std::collections::HashMap<char, char> = halfwidth_katakana().collect();
    let nfc = ComposingNormalizerBorrowed::new_nfc();
    let mut out = String::with_capacity(text.len() * 3);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let mapped = match c {
            ' ' => '\u{3000}',
            '!'..='~' => char::from_u32(u32::from(c) + 0xFEE0).unwrap_or(c),
            _ => match SIGNS.iter().find(|s| s.1 == c) {
                Some(&(full, _)) => full,
                None => table.get(&c).copied().unwrap_or(c),
            },
        };
        // A letter and a voicing mark make one voiced kana, if there is one.
        if let Some(&next @ ('\u{FF9E}' | '\u{FF9F}')) = chars.peek()
            && table.contains_key(&c)
        {
            let mark = if next == '\u{FF9E}' { '\u{3099}' } else { '\u{309A}' };
            let pair: String = [mapped, mark].into_iter().collect();
            let composed = nfc.normalize(&pair);
            if composed.chars().count() == 1 {
                out.push_str(&composed);
                chars.next();
                continue;
            }
        }
        out.push(mapped);
    }
    out
}

fn to_katakana(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{3041}'..='\u{3094}' | '\u{309D}' | '\u{309E}' => char::from_u32(u32::from(c) + 0x60).unwrap_or(c),
            _ => c,
        })
        .collect()
}

fn to_hiragana(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{30A1}'..='\u{30F4}' | '\u{30FD}' | '\u{30FE}' => {
                out.push(char::from_u32(u32::from(c) - 0x60).unwrap_or(c));
            }
            // Small ka and ke have no hiragana of their own here.
            '\u{30F5}' => out.push('\u{304B}'),
            '\u{30F6}' => out.push('\u{3051}'),
            // Voiced wa, wi, we and wo: the letter and the voicing mark.
            '\u{30F7}'..='\u{30FA}' => {
                out.push(['\u{308F}', '\u{3090}', '\u{3091}', '\u{3092}'][(u32::from(c) - 0x30F7) as usize]);
                out.push('\u{3099}');
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_as_measured() {
        assert_eq!(strip_marks("Ñandú ø Å ǅ ﬁ ẞ ḱ"), "Nandu ø A ǅ ﬁ ẞ k");
        assert_eq!(to_xml_hex("a\n\t<&>\u{7f}\u{80}é"), "a&#xA;&#x9;<&>&#x7F;&#x80;&#xE9;");
        assert_eq!(from_xml_hex("&#xe9;&#233;&#x41;&#X42;&#xZZ;&amp;&#x1f600;x"), "é&#233;A&#X42;&#xZZ;&amp;😀x");
        assert_eq!(to_halfwidth("ガパア　ＡＢ￥ー、。「」한"), "ｶ\u{ff9e}ﾊ\u{ff9f}ｱ AB¥ｰ､｡｢｣한");
        assert_eq!(to_fullwidth("ｶﾞﾊﾟｱ Ab¥ｰ､｡｢｣~\\"), "ガパア\u{3000}Ａｂ￥ー、。「」～＼");
        assert_eq!(to_katakana("ゝゞゔゕゖぁ ア"), "ヽヾヴゕゖァ ア");
        assert_eq!(to_hiragana("ヽヾヴヵヶァヷーカ "), "ゝゞゔかけぁわ\u{3099}ーか ");
    }

    #[test]
    fn unknown_and_unsupported_names() {
        assert_eq!(Transform::named(")kCFStringTransformToXMLHex"), Some(Transform::XmlHex));
        assert_eq!(Transform::named("Any-Hex/XML"), Some(Transform::XmlHex));
        assert_eq!(Transform::named(")kCFStringTransformLatinKatakana"), None);
        assert_eq!(Transform::named(")kCFStringTransformToUnicodeName"), None);
        assert_eq!(Transform::named("Bogus-Thing"), None);
    }
}
