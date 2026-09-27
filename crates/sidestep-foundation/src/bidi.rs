//! The Unicode Bidirectional Algorithm (UAX #9): each character's
//! embedding level and its paragraph's, for
//! `CFAttributedStringGetBidiLevelsAndResolvedDirections`.
//!
//! Paragraphs end after each paragraph separator. The text is taken as one
//! line per paragraph (rule L1 resets trailing whitespace at each
//! paragraph's end). Characters rule X9 removes (embedding controls,
//! boundary neutrals) take the level of the character after them, as
//! macOS reports them.

use icu_properties::CodePointMapData;
use icu_properties::props::{BidiClass, BidiMirroringGlyph, BidiPairedBracketType};

/// The bidi classes, by the algorithm's names for them.
#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum C {
    L,
    R,
    AL,
    EN,
    ES,
    ET,
    AN,
    CS,
    NSM,
    BN,
    B,
    S,
    WS,
    ON,
    LRE,
    LRO,
    RLE,
    RLO,
    PDF,
    LRI,
    RLI,
    FSI,
    PDI,
}

fn class(c: char) -> C {
    let b = CodePointMapData::<BidiClass>::new().get(c);
    match b {
        BidiClass::LeftToRight => C::L,
        BidiClass::RightToLeft => C::R,
        BidiClass::ArabicLetter => C::AL,
        BidiClass::EuropeanNumber => C::EN,
        BidiClass::EuropeanSeparator => C::ES,
        BidiClass::EuropeanTerminator => C::ET,
        BidiClass::ArabicNumber => C::AN,
        BidiClass::CommonSeparator => C::CS,
        BidiClass::NonspacingMark => C::NSM,
        BidiClass::BoundaryNeutral => C::BN,
        BidiClass::ParagraphSeparator => C::B,
        BidiClass::SegmentSeparator => C::S,
        BidiClass::WhiteSpace => C::WS,
        BidiClass::LeftToRightEmbedding => C::LRE,
        BidiClass::LeftToRightOverride => C::LRO,
        BidiClass::RightToLeftEmbedding => C::RLE,
        BidiClass::RightToLeftOverride => C::RLO,
        BidiClass::PopDirectionalFormat => C::PDF,
        BidiClass::LeftToRightIsolate => C::LRI,
        BidiClass::RightToLeftIsolate => C::RLI,
        BidiClass::FirstStrongIsolate => C::FSI,
        BidiClass::PopDirectionalIsolate => C::PDI,
        _ => C::ON,
    }
}

fn is_isolate_initiator(t: C) -> bool {
    matches!(t, C::LRI | C::RLI | C::FSI)
}

fn is_removed(t: C) -> bool {
    matches!(t, C::RLE | C::LRE | C::RLO | C::LRO | C::PDF | C::BN)
}

/// Each character's level and its paragraph's level.
pub(crate) struct Levels {
    pub(crate) levels: Vec<u8>,
    pub(crate) paragraphs: Vec<u8>,
}

/// Resolve `text`'s levels; `base` is the paragraphs' level, or `None` to
/// take each paragraph's from its first strong character.
pub(crate) fn resolve(text: &[char], base: Option<u8>) -> Levels {
    let classes: Vec<C> = text.iter().map(|&c| class(c)).collect();
    let mut out = Levels { levels: vec![0; text.len()], paragraphs: vec![0; text.len()] };
    let mut start = 0;
    while start < text.len() {
        let end = classes[start..].iter().position(|&t| t == C::B).map_or(text.len(), |i| start + i + 1);
        paragraph(text, &classes, start..end, base, &mut out);
        start = end;
    }
    out
}

/// Where the isolate initiator at `i` is matched (BD9), if it is.
fn matching_pdi(classes: &[C], i: usize, end: usize) -> Option<usize> {
    let mut depth = 1;
    for (j, &t) in classes.iter().enumerate().take(end).skip(i + 1) {
        if is_isolate_initiator(t) {
            depth += 1;
        } else if t == C::PDI {
            depth -= 1;
            if depth == 0 {
                return Some(j);
            }
        } else if t == C::B {
            return None;
        }
    }
    None
}

/// P2 and P3: 1 if the first strong character (isolates skipped) is right
/// to left, 0 otherwise.
fn first_strong_level(classes: &[C], range: std::ops::Range<usize>) -> u8 {
    let mut i = range.start;
    while i < range.end {
        match classes[i] {
            C::L => return 0,
            C::R | C::AL => return 1,
            t if is_isolate_initiator(t) => match matching_pdi(classes, i, range.end) {
                Some(j) => i = j,
                None => return 0,
            },
            _ => {}
        }
        i += 1;
    }
    0
}

const MAX_DEPTH: u8 = 125;

struct Entry {
    level: u8,
    over: Option<C>,
    isolate: bool,
}

fn paragraph(text: &[char], original: &[C], range: std::ops::Range<usize>, base: Option<u8>, out: &mut Levels) {
    let para = base.unwrap_or_else(|| first_strong_level(original, range.clone()));
    let mut types: Vec<C> = original[range.clone()].to_vec();
    let n = types.len();
    let offset = range.start;
    let mut levels = vec![para; n];

    // X1–X8: explicit levels and directions.
    let mut stack = vec![Entry { level: para, over: None, isolate: false }];
    let (mut overflow_isolates, mut overflow_embeddings, mut valid_isolates) = (0usize, 0usize, 0usize);
    for i in 0..n {
        let t = types[i];
        let top_level = stack.last().map_or(para, |e| e.level);
        let top_over = stack.last().and_then(|e| e.over);
        match t {
            C::RLE | C::LRE | C::RLO | C::LRO => {
                let rtl = matches!(t, C::RLE | C::RLO);
                let level = if rtl { (top_level + 1) | 1 } else { (top_level + 2) & !1 };
                levels[i] = top_level;
                if level <= MAX_DEPTH && overflow_isolates == 0 && overflow_embeddings == 0 {
                    let over = match t {
                        C::RLO => Some(C::R),
                        C::LRO => Some(C::L),
                        _ => None,
                    };
                    stack.push(Entry { level, over, isolate: false });
                } else if overflow_isolates == 0 {
                    overflow_embeddings += 1;
                }
            }
            C::RLI | C::LRI | C::FSI => {
                levels[i] = top_level;
                if let Some(o) = top_over {
                    types[i] = o;
                }
                let rtl = match t {
                    C::RLI => true,
                    C::LRI => false,
                    _ => {
                        let stop = matching_pdi(original, offset + i, range.end).unwrap_or(range.end);
                        first_strong_level(original, offset + i + 1..stop) == 1
                    }
                };
                let level = if rtl { (top_level + 1) | 1 } else { (top_level + 2) & !1 };
                if level <= MAX_DEPTH && overflow_isolates == 0 && overflow_embeddings == 0 {
                    valid_isolates += 1;
                    stack.push(Entry { level, over: None, isolate: true });
                } else {
                    overflow_isolates += 1;
                }
            }
            C::PDI => {
                if overflow_isolates > 0 {
                    overflow_isolates -= 1;
                } else if valid_isolates > 0 {
                    overflow_embeddings = 0;
                    while stack.last().is_some_and(|e| !e.isolate) {
                        stack.pop();
                    }
                    stack.pop();
                    valid_isolates -= 1;
                }
                let top = stack.last();
                levels[i] = top.map_or(para, |e| e.level);
                if let Some(o) = top.and_then(|e| e.over) {
                    types[i] = o;
                }
            }
            C::PDF => {
                levels[i] = top_level;
                if overflow_isolates == 0 {
                    if overflow_embeddings > 0 {
                        overflow_embeddings -= 1;
                    } else if stack.last().is_some_and(|e| !e.isolate) && stack.len() >= 2 {
                        stack.pop();
                    }
                }
            }
            C::B => levels[i] = para,
            C::BN => levels[i] = top_level,
            _ => {
                levels[i] = top_level;
                if let Some(o) = top_over {
                    types[i] = o;
                }
            }
        }
    }

    // X9 and X10: isolating run sequences of the remaining characters.
    let kept: Vec<usize> = (0..n).filter(|&i| !is_removed(original[offset + i])).collect();
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for &i in &kept {
        match runs.last_mut() {
            Some(run) if levels[*run.last().unwrap()] == levels[i] => run.push(i),
            _ => runs.push(vec![i]),
        }
    }
    let pdi_of = |i: usize| matching_pdi(original, offset + i, range.end).map(|j| j - offset);
    let mut sequences: Vec<Vec<usize>> = Vec::new();
    let mut used = vec![false; runs.len()];
    for r in 0..runs.len() {
        if used[r] {
            continue;
        }
        // A run starting with a PDI that matches an initiator continues an
        // earlier sequence, which took it.
        let mut sequence = Vec::new();
        let mut current = r;
        loop {
            used[current] = true;
            sequence.extend_from_slice(&runs[current]);
            let last = *runs[current].last().unwrap();
            let Some(pdi) = is_isolate_initiator(original[offset + last]).then(|| pdi_of(last)).flatten() else {
                break;
            };
            match runs.iter().position(|run| run[0] == pdi) {
                Some(next) if !used[next] => current = next,
                _ => break,
            }
        }
        sequences.push(sequence);
    }

    for sequence in &sequences {
        resolve_sequence(text, original, &mut types, &levels, sequence, &kept, para, offset, range.end);
    }

    // I1 and I2.
    for &i in &kept {
        let level = levels[i];
        levels[i] = match (level.is_multiple_of(2), types[i]) {
            (true, C::R) => level + 1,
            (true, C::AN | C::EN) => level + 2,
            (false, C::L | C::EN | C::AN) => level + 1,
            _ => level,
        };
    }

    // L1: separators, and whitespace before them or at the paragraph's
    // end, go back to the paragraph's level.
    let mut trailing = true;
    for i in (0..n).rev() {
        let t = original[offset + i];
        match t {
            C::S | C::B => {
                levels[i] = para;
                trailing = true;
            }
            C::WS | C::LRI | C::RLI | C::FSI | C::PDI if trailing => levels[i] = para,
            t if is_removed(t) => {}
            _ => trailing = false,
        }
    }

    // Removed characters take the level of the character after them.
    let mut next = para;
    for i in (0..n).rev() {
        if is_removed(original[offset + i]) {
            levels[i] = next;
        } else {
            next = levels[i];
        }
    }

    out.levels[range.clone()].copy_from_slice(&levels);
    out.paragraphs[range].fill(para);
}

fn strong_direction(level: u8) -> C {
    if level.is_multiple_of(2) { C::L } else { C::R }
}

#[allow(clippy::too_many_arguments)]
fn resolve_sequence(
    text: &[char],
    original: &[C],
    types: &mut [C],
    levels: &[u8],
    sequence: &[usize],
    kept: &[usize],
    para: u8,
    offset: usize,
    _end: usize,
) {
    let level = levels[sequence[0]];
    let e = strong_direction(level);
    // sos and eos: the higher of the sequence's level and its neighbours'.
    let first = sequence[0];
    let before = kept.iter().rev().find(|&&k| k < first).map_or(para, |&k| levels[k]);
    let sos = strong_direction(level.max(before));
    let last = *sequence.last().unwrap();
    // A sequence ending in an isolate initiator ends there because it has
    // no matching PDI.
    let after = if is_isolate_initiator(original[offset + last]) {
        para
    } else {
        kept.iter().find(|&&k| k > last).map_or(para, |&k| levels[k])
    };
    let eos = strong_direction(level.max(after));
    let original_types: Vec<C> = sequence.iter().map(|&i| types[i]).collect();
    let mut t: Vec<C> = original_types.clone();
    let m = t.len();

    // W1: marks take the type before them.
    let mut prev = sos;
    for x in t.iter_mut() {
        if *x == C::NSM {
            *x = if is_isolate_initiator(prev) || prev == C::PDI { C::ON } else { prev };
        }
        prev = *x;
    }
    // W2: European numbers after Arabic letters are Arabic.
    let mut last_strong = sos;
    for x in t.iter_mut() {
        match *x {
            C::L | C::R | C::AL => last_strong = *x,
            C::EN if last_strong == C::AL => *x = C::AN,
            _ => {}
        }
    }
    // W3.
    for x in t.iter_mut() {
        if *x == C::AL {
            *x = C::R;
        }
    }
    // W4: a single separator between numbers of a kind.
    for k in 1..m.saturating_sub(1) {
        match (t[k - 1], t[k], t[k + 1]) {
            (C::EN, C::ES | C::CS, C::EN) => t[k] = C::EN,
            (C::AN, C::CS, C::AN) => t[k] = C::AN,
            _ => {}
        }
    }
    // W5: terminators next to European numbers.
    let mut k = 0;
    while k < m {
        if t[k] == C::ET {
            let start = k;
            while k < m && t[k] == C::ET {
                k += 1;
            }
            let touches = (start > 0 && t[start - 1] == C::EN) || (k < m && t[k] == C::EN);
            if touches {
                for x in &mut t[start..k] {
                    *x = C::EN;
                }
            }
        } else {
            k += 1;
        }
    }
    // W6.
    for x in t.iter_mut() {
        if matches!(*x, C::ES | C::ET | C::CS) {
            *x = C::ON;
        }
    }
    // W7: European numbers after left-to-right text are left to right.
    let mut last_strong = sos;
    for x in t.iter_mut() {
        match *x {
            C::L | C::R => last_strong = *x,
            C::EN if last_strong == C::L => *x = C::L,
            _ => {}
        }
    }

    // N0: paired brackets.
    let brackets = bracket_pairs(text, &t, sequence);
    for (open, close) in brackets {
        let strong_of = |x: C| match x {
            C::L => Some(C::L),
            C::R | C::EN | C::AN => Some(C::R),
            _ => None,
        };
        let inside: Vec<C> = t[open + 1..close].iter().filter_map(|&x| strong_of(x)).collect();
        let direction = if inside.contains(&e) {
            Some(e)
        } else if let Some(&other) = inside.first() {
            let context = t[..open].iter().rev().find_map(|&x| strong_of(x)).unwrap_or(sos);
            Some(if context == other { other } else { e })
        } else {
            None
        };
        if let Some(d) = direction {
            for at in [open, close] {
                t[at] = d;
                // Marks after a bracket follow it.
                let mut k = at + 1;
                while k < m && original_types[k] == C::NSM {
                    t[k] = d;
                    k += 1;
                }
            }
        }
    }

    // N1 and N2: neutrals between strong types of one direction take it,
    // others the embedding direction.
    let is_ni = |x: C| matches!(x, C::B | C::S | C::WS | C::ON | C::LRI | C::RLI | C::FSI | C::PDI);
    let direction_of = |x: C| match x {
        C::L => C::L,
        C::R | C::EN | C::AN => C::R,
        other => other,
    };
    let mut k = 0;
    while k < m {
        if !is_ni(t[k]) {
            k += 1;
            continue;
        }
        let start = k;
        while k < m && is_ni(t[k]) {
            k += 1;
        }
        let before = if start == 0 { sos } else { direction_of(t[start - 1]) };
        let after = if k == m { eos } else { direction_of(t[k]) };
        let d = if before == after { before } else { e };
        for x in &mut t[start..k] {
            *x = d;
        }
    }

    for (k, &i) in sequence.iter().enumerate() {
        types[i] = t[k];
    }
}

/// BD16: the sequence's bracket pairs, as positions in it, in the order of
/// their opening brackets.
fn bracket_pairs(text: &[char], t: &[C], sequence: &[usize]) -> Vec<(usize, usize)> {
    let data = CodePointMapData::<BidiMirroringGlyph>::new();
    // Canonical equivalents count as the same bracket.
    let canonical = |c: char| match c {
        '\u{2329}' => '\u{3008}',
        '\u{232A}' => '\u{3009}',
        other => other,
    };
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut pairs = Vec::new();
    for (k, &i) in sequence.iter().enumerate() {
        if t[k] != C::ON {
            continue;
        }
        let c = text[i];
        let props = data.get(c);
        match props.paired_bracket_type {
            BidiPairedBracketType::Open => {
                if stack.len() == 63 {
                    break;
                }
                let closing = props.mirroring_glyph.unwrap_or(c);
                stack.push((canonical(closing), k));
            }
            BidiPairedBracketType::Close => {
                if let Some(at) = stack.iter().rposition(|&(want, _)| want == canonical(c)) {
                    pairs.push((stack[at].1, k));
                    stack.truncate(at);
                }
            }
            _ => {}
        }
    }
    pairs.sort_unstable();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(text: &str, base: Option<u8>) -> (Vec<u8>, Vec<u8>) {
        let chars: Vec<char> = text.chars().collect();
        let r = resolve(&chars, base);
        (r.levels, r.paragraphs)
    }

    #[test]
    fn levels_as_measured_on_macos() {
        assert_eq!(levels("abc שלום def", None).0, [0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0]);
        assert_eq!(levels("abc שלום def", Some(1)).0, [2, 2, 2, 1, 1, 1, 1, 1, 1, 2, 2, 2]);
        assert_eq!(levels("שלום abc", None).0, [1, 1, 1, 1, 1, 2, 2, 2]);
        assert_eq!(levels("abc 123 שלום", Some(1)).0, [2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1]);
        assert_eq!(levels("(abc)", Some(1)).0, [1, 2, 2, 2, 1]);
        assert_eq!(levels("שלום 123!", Some(0)).0, [1, 1, 1, 1, 1, 2, 2, 2, 0]);
        assert_eq!(levels("a\nש", None), (vec![0, 0, 1], vec![0, 0, 1]));
        assert_eq!(levels("a\nש", Some(1)).0, [2, 1, 1]);
        assert_eq!(levels("مرحبا 12", None).0, [1, 1, 1, 1, 1, 1, 2, 2]);
        assert_eq!(levels("a\u{202b}b\u{202c}c", None).0, [0, 2, 2, 0, 0]);
        assert_eq!(levels("a\u{202b}b\u{202c}c", Some(1)).0, [2, 4, 4, 2, 2]);
        assert_eq!(levels("\u{2067}ab\u{2069}", None).0, [0, 2, 2, 0]);
        assert_eq!(levels("\u{2067}ab\u{2069}", Some(1)).0, [1, 4, 4, 1]);
        assert_eq!(levels("123 שלום", None), (vec![2, 2, 2, 1, 1, 1, 1, 1], vec![1; 8]));
        assert_eq!(levels("abc ", Some(1)).0, [2, 2, 2, 1]);
        assert_eq!(levels("abc (שלום) d", Some(1)).0, [2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 2]);
        assert_eq!(levels("a[ש]b", None).0, [0, 0, 1, 0, 0]);
        assert_eq!(levels("a[ש]b", Some(1)).0, [2, 1, 1, 1, 2]);
        assert_eq!(levels("1+2=3", Some(1)).0, [2, 2, 2, 1, 2]);
        assert_eq!(levels("\u{200f}abc", None).0, [1, 2, 2, 2]);
        assert_eq!(levels("abc\u{2029}def ש", Some(1)).0, [2, 2, 2, 1, 2, 2, 2, 1, 1]);
    }
}
