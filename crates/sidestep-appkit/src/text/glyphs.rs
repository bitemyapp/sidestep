//! Lines as CoreText describes them: glyph runs with every glyph's id,
//! position, advance and the index of the character it came from, laid out
//! by the same code as string drawing ([`layout`]: parley's shaping, font
//! fallback, bidi and line breaking), so a `CTLine` and the same text drawn
//! by AppKit are the same glyphs in the same places.
//!
//! [`glyph_lines`] lays text out on one line (a `CTLine`: separators and
//! other control characters become zero-width glyphs, as in CoreText, and
//! a tab reaches the next tab stop) or broken to a width (what a
//! typesetter suggests and a framesetter fills a frame with). Positions are
//! from the line's start, y up, in points; indices count UTF-16 units of
//! the text laid out. Everything here is plain data, so lines can be kept
//! and drawn on any thread.

use std::ops::Range;
use std::sync::Arc;

use parley::{FontData, PositionedLayoutItem};

use super::fonts::Synth;
use super::layout::{self, Attrs, Direction, LineBreak, Run, Settings};

/// A run of glyphs in one face, size, direction and set of attributes.
#[derive(Clone, Debug)]
pub(crate) struct GlyphRunData {
    /// The attributes' index.
    pub attrs: u32,
    pub font: FontData,
    /// Variation coordinates and what drawing synthesizes.
    pub coords: Arc<[i16]>,
    pub synth: Synth,
    pub size: f32,
    pub rtl: bool,
    pub glyphs: Vec<u16>,
    /// From the line's start, y up.
    pub positions: Vec<(f64, f64)>,
    pub advances: Vec<f64>,
    /// UTF-16 index of each glyph's character.
    pub indices: Vec<usize>,
    /// What each glyph's character is to a line.
    pub class: Vec<Class>,
    /// The UTF-16 range of the run's characters.
    pub range: Range<usize>,
}

/// What a glyph's character is to a line: blank or not (whitespace, a
/// control character, a separator), and how justification and hanging
/// punctuation treat it (measured on macOS).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Class(u8);

impl Class {
    const BLANK: u8 = 1;
    const SPACE: u8 = 2;
    const TAB: u8 = 4;
    const HANGS_AFTER: u8 = 8;
    const HANGS_BEFORE: u8 = 16;

    /// The class of a cluster starting with `c`, blank if `blank`.
    pub fn of(c: char, blank: bool) -> Class {
        let mut bits = if blank { Self::BLANK } else { 0 };
        match c {
            ' ' | '\u{a0}' => bits |= Self::SPACE,
            '\t' => bits |= Self::TAB,
            _ => {}
        }
        // Quotes hang either way; closing punctuation after the end.
        if matches!(
            c,
            '\'' | '"' | '\u{2018}' | '\u{2019}' | '\u{201c}' | '\u{ab}' | '\u{bb}' | '\u{2039}' | '\u{203a}'
        ) {
            bits |= Self::HANGS_AFTER | Self::HANGS_BEFORE;
        } else if matches!(c, '.' | ',' | '-' | '\u{201d}') {
            bits |= Self::HANGS_AFTER;
        } else if c == '\u{201e}' {
            bits |= Self::HANGS_BEFORE;
        }
        Class(bits)
    }

    pub const fn blank() -> Class {
        Class(Self::BLANK)
    }

    /// Whitespace, a control character or a separator.
    pub fn is_blank(self) -> bool {
        self.0 & Self::BLANK != 0
    }

    /// A space between words (U+0020, U+00A0), which justification widens
    /// first.
    pub fn is_space(self) -> bool {
        self.0 & Self::SPACE != 0
    }

    /// A tab, which justification leaves alone.
    pub fn is_tab(self) -> bool {
        self.0 & Self::TAB != 0
    }

    /// Punctuation that hangs past the end of a line.
    pub fn hangs_after(self) -> bool {
        self.0 & Self::HANGS_AFTER != 0
    }

    /// Punctuation that hangs before the start of a line.
    pub fn hangs_before(self) -> bool {
        self.0 & Self::HANGS_BEFORE != 0
    }
}

/// A laid-out line.
#[derive(Clone, Debug, Default)]
pub(crate) struct GlyphLine {
    pub runs: Vec<GlyphRunData>,
    /// The typographic width, trailing whitespace included.
    pub width: f64,
    /// The UTF-16 range of the line's characters.
    pub range: Range<usize>,
}

impl GlyphLine {
    /// The line with only the glyphs of characters before UTF-16 unit
    /// `end`.
    pub fn cut(&self, end: usize) -> GlyphLine {
        let runs = self
            .runs
            .iter()
            .filter_map(|r| {
                let keep: Vec<usize> = (0..r.glyphs.len()).filter(|&k| r.indices[k] < end).collect();
                if keep.is_empty() {
                    return None;
                }
                let pick = |v: &[f64]| keep.iter().map(|&k| v[k]).collect::<Vec<_>>();
                Some(GlyphRunData {
                    glyphs: keep.iter().map(|&k| r.glyphs[k]).collect(),
                    positions: keep.iter().map(|&k| r.positions[k]).collect(),
                    advances: pick(&r.advances),
                    indices: keep.iter().map(|&k| r.indices[k]).collect(),
                    class: keep.iter().map(|&k| r.class[k]).collect(),
                    range: r.range.start..r.range.end.min(end),
                    ..r.clone()
                })
            })
            .collect::<Vec<_>>();
        let width = runs.iter().flat_map(|r| r.advances.iter()).sum();
        GlyphLine { runs, width, range: self.range.start..self.range.end.min(end) }
    }
}

/// How [`glyph_lines`] breaks text.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Breaking {
    /// All on one line.
    None,
    /// Into lines this wide, at word boundaries, a word too long for a line
    /// broken where it reaches the end.
    Words(f32),
    /// Into lines this wide, between any two clusters.
    Clusters(f32),
}

/// UTF-16 offsets of `text`'s bytes (at character boundaries).
fn utf16_offsets(text: &str) -> Vec<u32> {
    let mut out = vec![0u32; text.len() + 1];
    let mut unit = 0u32;
    for (i, c) in text.char_indices() {
        out[i] = unit;
        unit += c.len_utf16() as u32;
    }
    out[text.len()] = unit;
    // Bytes inside characters take their character's offset.
    let mut last = 0;
    for (i, slot) in out.iter_mut().enumerate() {
        if text.is_char_boundary(i) {
            last = *slot;
        } else {
            *slot = last;
        }
    }
    out
}

/// Lay `text` out as `breaking` says, with `runs` over it indexing
/// `attrs`, in `direction`. There is at least one line.
pub(crate) fn glyph_lines(
    text: &str,
    attrs: &[Attrs],
    runs: &[Run],
    breaking: Breaking,
    direction: Direction,
) -> Vec<GlyphLine> {
    glyph_lines_at(text, attrs, runs, breaking, direction, 0.0)
}

/// [`glyph_lines`] for lines that start `offset` points along: tab stops
/// are measured from `offset` before the lines' starts, which positions
/// stay relative to, and lines broken to a width have that much room from
/// there.
pub(crate) fn glyph_lines_at(
    text: &str,
    attrs: &[Attrs],
    runs: &[Run],
    breaking: Breaking,
    direction: Direction,
    offset: f32,
) -> Vec<GlyphLine> {
    let offsets = utf16_offsets(text);
    let total = offsets[text.len()] as usize;
    if text.is_empty() {
        return vec![GlyphLine { runs: Vec::new(), width: 0.0, range: 0..0 }];
    }
    let (mode, width) = match breaking {
        Breaking::None => (LineBreak::Clip, f32::INFINITY),
        Breaking::Words(w) => (LineBreak::WordWrap, w),
        Breaking::Clusters(w) => (LineBreak::CharWrap, w),
    };
    let wraps = !matches!(breaking, Breaking::None);
    let settings = Settings { mode, wraps, direction, context: None, own_emoji: true };
    let para = &attrs[runs.first().map_or(0, |r| r.attrs) as usize].paragraph;
    super::with_ctx(|ctx| {
        let (mut parley_layout, shift) = layout::build(ctx, text, attrs, runs, &settings);
        layout::break_and_tab(&mut parley_layout, text, shift, para, width + offset, offset, offset);
        let mut lines: Vec<GlyphLine> = Vec::new();
        // All on one line: the lines a mandatory break made follow each
        // other.
        let mut pen0 = 0.0f64;
        for line in parley_layout.lines() {
            let m = line.metrics();
            let origin = f64::from(m.inline_min_coord + m.offset);
            let mut out =
                if wraps || lines.is_empty() { GlyphLine::default() } else { lines.pop().unwrap_or_default() };
            let text_range = line.text_range();
            let (start, end) = (text_range.start.saturating_sub(shift), text_range.end.saturating_sub(shift));
            let (u0, u1) = (offsets[start.min(text.len())] as usize, offsets[end.min(text.len())] as usize);
            out.range = if out.runs.is_empty() && out.range.is_empty() { u0..u1 } else { out.range.start.min(u0)..u1 };
            // Glyphs of each parley run taken by earlier glyph runs.
            let mut taken = (usize::MAX, 0usize);
            // What parley advanced for glyphs that take no room here
            // (control characters), which later runs' offsets include.
            let mut drift = 0.0f64;
            for item in line.items() {
                let glyph_run = match item {
                    PositionedLayoutItem::InlineBox(b) => {
                        // A tab's box: the tab's glyph reaches over it, to
                        // the stop (which what took no room here doesn't
                        // move).
                        if let Some(last) = out.runs.last_mut()
                            && let Some(advance) = last.advances.last_mut()
                        {
                            *advance += f64::from(b.width) + drift;
                            drift = 0.0;
                        }
                        continue;
                    }
                    PositionedLayoutItem::GlyphRun(g) => g,
                };
                let run = glyph_run.run();
                let count = glyph_run.glyphs().count();
                let first = if taken.0 == run.index() { taken.1 } else { 0 };
                taken = (run.index(), first + count);
                let hidden = run.font_size() <= layout::HIDDEN_SIZE;
                let style = glyph_run.style().brush.0;
                let a = &attrs[style as usize];
                let mark =
                    if shift > 0 && run.text_range().start < shift { layout::mark_glyphs(run, shift) } else { 0..0 };
                let size = if hidden { a.font.size } else { run.font_size() };
                let synthesis = run.synthesis();
                // CoreText doesn't slant for `NSObliqueness` (measured on
                // macOS), only for a face it synthesizes.
                let synth =
                    Synth { embolden: synthesis.embolden(), skew: synthesis.skew().unwrap_or(0.0), stroke: 0.0 };
                let x0 = f64::from(glyph_run.offset()) - origin + pen0 - drift;
                let mut x = x0;
                let mut index = 0usize;
                let rtl = run.is_rtl();
                // The characters of clusters with no glyphs of their own (a
                // ligature's later ones, a separator) go to the run of the
                // glyph next to them: the one before them, or in a
                // right-to-left run, the one after.
                let mut pending: Option<Range<usize>> = None;
                let mut pushed: Option<usize> = None;
                for cluster in run.visual_clusters() {
                    let range = cluster.text_range();
                    let units = || {
                        let (b0, b1) = ((range.start - shift).min(text.len()), (range.end - shift).min(text.len()));
                        offsets[b0] as usize..offsets[b1] as usize
                    };
                    let chars = text.get(range.start.saturating_sub(shift)..range.end.saturating_sub(shift));
                    let control = chars.is_some_and(|t| !t.is_empty() && t.chars().all(char::is_control));
                    let blank = cluster.is_space_or_nbsp()
                        || hidden
                        || chars.is_some_and(|t| t.chars().all(|c| c.is_whitespace() || c.is_control()));
                    let class = Class::of(chars.and_then(|t| t.chars().next()).unwrap_or(' '), blank);
                    let glyph_count = cluster.glyphs().count();
                    if glyph_count == 0 {
                        let inside = if rtl {
                            first <= index && index < first + count
                        } else {
                            first < index && index <= first + count
                        };
                        if inside && range.start >= shift {
                            let u = units();
                            pending = Some(pending.map_or(u.clone(), |p| p.start.min(u.start)..p.end.max(u.end)));
                            if !rtl && let Some(at) = pushed {
                                let data = &mut out.runs[at];
                                let p = pending.take().unwrap_or(u);
                                data.range = data.range.start.min(p.start)..data.range.end.max(p.end);
                            }
                        }
                        continue;
                    }
                    for g in cluster.glyphs() {
                        let i = index;
                        index += 1;
                        if i < first || i >= first + count {
                            continue;
                        }
                        // Control characters and hidden runs take no room,
                        // and a control character missing from the font
                        // is the space, as in CoreText.
                        let zero = hidden || control;
                        let advance = if zero { 0.0 } else { f64::from(g.advance) };
                        if zero {
                            drift += f64::from(g.advance);
                        }
                        if range.start < shift || mark.contains(&i) {
                            x += advance;
                            continue;
                        }
                        let unit = units();
                        let id = if control && g.id == 0 { space_glyph(run.font()).unwrap_or(0) } else { g.id as u16 };
                        let data = open_run(&mut out.runs, style, run, size, &synth, rtl);
                        data.glyphs.push(id);
                        data.positions.push((x + f64::from(g.x), f64::from(a.baseline_offset) - f64::from(g.y)));
                        data.advances.push(advance);
                        data.indices.push(unit.start);
                        data.class.push(class);
                        let mut unit = unit;
                        if let Some(p) = pending.take() {
                            unit = unit.start.min(p.start)..unit.end.max(p.end);
                        }
                        data.range = if data.glyphs.len() == 1 {
                            unit
                        } else {
                            data.range.start.min(unit.start)..data.range.end.max(unit.end)
                        };
                        pushed = Some(out.runs.len() - 1);
                        x += advance;
                    }
                }
                if let (Some(p), Some(at)) = (pending, pushed) {
                    let data = &mut out.runs[at];
                    data.range = data.range.start.min(p.start)..data.range.end.max(p.end);
                }
            }
            add_separators(&mut out, text, &offsets, u0..u1, attrs);
            out.width = out.runs.iter().flat_map(|r| r.advances.iter()).sum::<f64>();
            pen0 = if wraps { 0.0 } else { out.width };
            lines.push(out);
        }
        if lines.is_empty() {
            lines.push(GlyphLine { runs: Vec::new(), width: 0.0, range: 0..total });
        }
        ctx.scratch = parley_layout;
        lines
    })
}

/// The glyph a face has for the space, if any.
fn space_glyph(font: &FontData) -> Option<u16> {
    glyph_for(font, ' ')
}

/// The glyph a face maps `c` to, if any.
fn glyph_for(font: &FontData, c: char) -> Option<u16> {
    use skrifa::MetadataProvider;
    let file = skrifa::FontRef::from_index(font.data.data(), font.index).ok()?;
    file.charmap().map(c).map(|g| g.to_u32() as u16)
}

/// Line and paragraph separators, which parley lays out with no glyph, as
/// glyphs taking no room after the character before them: the font's own
/// glyph for them if it has one, else its space, as CoreText gives them.
fn add_separators(line: &mut GlyphLine, text: &str, offsets: &[u32], units: Range<usize>, attrs: &[Attrs]) {
    for (byte, c) in text.char_indices() {
        if !matches!(c, '\u{2028}' | '\u{2029}') {
            continue;
        }
        let unit = offsets[byte] as usize;
        if !units.contains(&unit) || line.runs.iter().any(|r| r.indices.contains(&unit)) {
            continue;
        }
        // The glyph of the character before it, or the line's first.
        let before = line
            .runs
            .iter()
            .enumerate()
            .flat_map(|(r, run)| run.indices.iter().enumerate().map(move |(k, &i)| (i, r, k)))
            .filter(|&(i, ..)| i < unit)
            .max_by_key(|&(i, ..)| i);
        let (r, at) = match before {
            Some((_, r, k)) => (r, if line.runs[r].rtl { k } else { k + 1 }),
            None if !line.runs.is_empty() => (0, 0),
            None => continue,
        };
        let run = &mut line.runs[r];
        let x = match at.checked_sub(1) {
            Some(k) if !run.rtl => run.positions[k].0 + run.advances[k],
            _ => run.positions.get(at).map_or(0.0, |p| p.0),
        };
        let y = attrs.get(run.attrs as usize).map_or(0.0, |a| f64::from(a.baseline_offset));
        let glyph = glyph_for(&run.font, c).or_else(|| space_glyph(&run.font)).unwrap_or(0);
        run.glyphs.insert(at, glyph);
        run.positions.insert(at, (x, y));
        run.advances.insert(at, 0.0);
        run.indices.insert(at, unit);
        run.class.insert(at, Class::blank());
        run.range = run.range.start.min(unit)..run.range.end.max(unit + 1);
    }
}

/// The run glyphs of `style` in `run`'s face go into: the last one if it's
/// theirs, or a new one.
fn open_run<'a>(
    runs: &'a mut Vec<GlyphRunData>,
    style: u32,
    run: &parley::Run<'_, super::Brush>,
    size: f32,
    synth: &Synth,
    rtl: bool,
) -> &'a mut GlyphRunData {
    let font = run.font();
    let coords = run.normalized_coords();
    let same = runs.last().is_some_and(|last| {
        last.attrs == style
            && last.font.data.id() == font.data.id()
            && last.font.index == font.index
            && last.size.to_bits() == size.to_bits()
            && last.rtl == rtl
            && *last.coords == *coords
            && last.synth == *synth
    });
    if !same {
        runs.push(GlyphRunData {
            attrs: style,
            font: font.clone(),
            coords: coords.into(),
            synth: *synth,
            size,
            rtl,
            glyphs: Vec::new(),
            positions: Vec::new(),
            advances: Vec::new(),
            indices: Vec::new(),
            class: Vec::new(),
            range: 0..0,
        });
    }
    let last = runs.len() - 1;
    &mut runs[last]
}
