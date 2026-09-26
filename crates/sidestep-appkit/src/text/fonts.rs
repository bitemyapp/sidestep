//! Fonts: the system's families, faces resolved from what `NSFont` asks
//! for, and the registry that names faces to the render thread.
//!
//! fontique finds the system's fonts through fontconfig, which it loads at
//! run time with `dlopen`, so the desktop's configuration decides what
//! `sans-serif`, `monospace` and `system-ui` mean and which families fill
//! in for missing glyphs. Without fontconfig, the usual font directories
//! are scanned instead and well-known families stand in for the generic
//! ones; fallback for other scripts then only covers what those families
//! do. `SIDESTEP_FONT` and `SIDESTEP_MONO_FONT` name font files that take
//! the place of the system and monospaced fonts.
//!
//! Opening the collection reads fontconfig's configuration and cache, which
//! takes some milliseconds, so [`prewarm`] starts it on a background thread
//! as soon as a program first uses `NSApplication`, `NSColor`, `NSFont` or
//! `NSParagraphStyle` (their class loaders call it); the first measurement
//! waits for it only if it hasn't finished yet.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, OnceLock, RwLock};

use parley::fontique::{
    Attributes, Blob, Collection, CollectionOptions, FontStyle, FontWeight, FontWidth, GenericFamily, QueryFamily,
    QueryStatus, SourceCache,
};
use parley::{FontContext, FontData};
use skrifa::MetadataProvider;
use skrifa::instance::{Location, Size};
use skrifa::string::StringId;

/// The face a [`FontSpec`] names when it names no family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Design {
    /// The desktop's interface font.
    Default,
    Monospaced,
    Serif,
    /// Rounded if the desktop has such a family; the interface font if not.
    Rounded,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Family {
    System(Design),
    Named(Arc<str>),
}

/// What an `NSFont` or `NSFontDescriptor` asks for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FontSpec {
    pub family: Family,
    /// CSS weight, 1 to 1000.
    pub weight: f32,
    pub italic: bool,
    /// Width relative to normal (0.5 to 2).
    pub stretch: f32,
    /// Point size; 0 means the default for the role.
    pub size: f64,
    /// Digits of equal width, as in `monospacedDigitSystemFontOfSize:weight:`.
    pub tabular_digits: bool,
    /// OpenType features turned on or off, from a descriptor's feature
    /// settings.
    pub features: Option<Features>,
    /// The spec names a family or font the system doesn't have, so no font
    /// matches it (`fontWithDescriptor:size:` gives nil); `family` holds
    /// the name.
    pub missing: bool,
}

/// OpenType feature tags and values.
pub(crate) type Features = Arc<[([u8; 4], u16)]>;

impl FontSpec {
    pub fn system(design: Design, size: f64) -> Self {
        FontSpec {
            family: Family::System(design),
            weight: 400.0,
            italic: false,
            stretch: 1.0,
            size,
            tabular_digits: false,
            features: None,
            missing: false,
        }
    }

    pub(crate) fn key(&self) -> FaceKey {
        FaceKey {
            family: self.family.clone(),
            weight: self.weight.to_bits(),
            italic: self.italic,
            stretch: self.stretch.to_bits(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct FaceKey {
    family: Family,
    weight: u32,
    italic: bool,
    stretch: u32,
}

/// Vertical and other metrics of a face, per point of size. Distances
/// follow AppKit's signs: y up, so `descent` and `underline_position` are
/// negative.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Metrics {
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
    pub cap_height: f32,
    pub x_height: f32,
    pub underline_position: f32,
    pub underline_thickness: f32,
    /// Where a strikethrough's top is above the baseline, and how thick.
    pub strikeout_position: f32,
    pub strikeout_thickness: f32,
    /// The union of the glyphs' boxes: x0, y0, x1, y1.
    pub bounds: [f32; 4],
    pub italic_angle: f32,
    pub max_advance: f32,
}

/// A face resolved from a [`FontSpec`], with what `NSFont` reports about it.
#[derive(Debug)]
pub(crate) struct Face {
    /// The family to lay text out in, as the collection names it.
    pub family: Arc<str>,
    pub postscript_name: Arc<str>,
    pub full_name: Arc<str>,
    /// What layout asks for to get this face: the spec's width and style,
    /// and the face's own weight unless it needs emboldening (see
    /// [`load_face`]).
    pub weight: f32,
    pub italic: bool,
    pub stretch: f32,
    pub metrics: Metrics,
    pub fixed_pitch: bool,
    pub glyph_count: u32,
    /// The font file, and the variation settings the face was matched with.
    pub font: Option<FontData>,
    pub variations: Vec<(skrifa::Tag, f32)>,
}

/// The families behind AppKit's font roles, and the prototype font
/// collection each thread's layout context clones. Clones share the
/// system's fonts and loaded font files.
pub(crate) struct Shared {
    fcx: Mutex<FontContext>,
    system: Arc<str>,
    mono: Arc<str>,
    serif: Arc<str>,
    /// Family names with spaces removed and lowercased, for PostScript
    /// names such as `DejaVuSans-Bold`; built on first use.
    squashed: OnceLock<HashMap<String, Arc<str>>>,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

pub(crate) fn shared() -> &'static Shared {
    SHARED.get_or_init(Shared::open)
}

/// Open the font collection on a background thread, ahead of first use.
pub(crate) fn prewarm() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = std::thread::Builder::new().name("sidestep-fonts".into()).spawn(|| {
            shared();
        });
    });
}

/// Where fonts live when fontconfig can't be loaded.
const FONT_DIRS: &[&str] = &["/usr/share/fonts", "/usr/local/share/fonts"];
const SANS: &[&str] = &["Cantarell", "Noto Sans", "DejaVu Sans", "Liberation Sans", "FreeSans"];
const MONO: &[&str] = &["Noto Sans Mono", "DejaVu Sans Mono", "Liberation Mono", "FreeMono"];
const SERIF: &[&str] = &["Noto Serif", "DejaVu Serif", "Liberation Serif", "FreeSerif"];

impl Shared {
    fn open() -> Shared {
        // fontique asks fontconfig for the generic families as it opens the
        // collection, and panics if fontconfig has no fonts at all; the
        // font directories are scanned instead then.
        let open = |system_fonts| Collection::new(CollectionOptions { shared: true, system_fonts });
        let mut collection = std::panic::catch_unwind(|| open(true)).unwrap_or_else(|_| open(false));
        let mut source_cache = SourceCache::new_shared();
        if collection.family_names().next().is_none() {
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
            let user = home.iter().flat_map(|h| [h.join(".local/share/fonts"), h.join(".fonts")]);
            collection.load_fonts_from_paths(FONT_DIRS.iter().map(std::path::PathBuf::from).chain(user));
            for (generic, names) in [
                (GenericFamily::SansSerif, SANS),
                (GenericFamily::SystemUi, SANS),
                (GenericFamily::Monospace, MONO),
                (GenericFamily::Serif, SERIF),
            ] {
                let ids: Vec<_> = names.iter().filter_map(|n| collection.family_id(n)).collect();
                collection.set_generic_families(generic, ids.into_iter());
            }
        }
        let system_override = register_file(&mut collection, "SIDESTEP_FONT");
        let mono_override = register_file(&mut collection, "SIDESTEP_MONO_FONT");
        let mut first = |generics: &[GenericFamily], fallback: &str| -> Arc<str> {
            for &generic in generics {
                let id = collection.generic_families(generic).next();
                if let Some(name) = id.and_then(|id| collection.family_name(id)) {
                    return name.into();
                }
            }
            fallback.into()
        };
        let system = system_override
            .unwrap_or_else(|| first(&[GenericFamily::SystemUi, GenericFamily::SansSerif], "sans-serif"));
        let mono = mono_override.unwrap_or_else(|| first(&[GenericFamily::Monospace], "monospace"));
        let serif = first(&[GenericFamily::Serif], "serif");
        // Warm the source cache with the interface font, which almost
        // every program draws with first.
        let mut query = collection.query(&mut source_cache);
        query.set_families([QueryFamily::Named(&system)]);
        query.matches_with(|_| QueryStatus::Stop);
        drop(query);
        Shared {
            fcx: Mutex::new(FontContext { collection, source_cache }),
            system,
            mono,
            serif,
            squashed: OnceLock::new(),
        }
    }

    /// A font context for another thread, sharing this one's fonts.
    pub fn font_context(&self) -> FontContext {
        self.fcx.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn family_for(&self, design: Design) -> &Arc<str> {
        match design {
            Design::Default | Design::Rounded => &self.system,
            Design::Monospaced => &self.mono,
            Design::Serif => &self.serif,
        }
    }

    fn squashed(&self, fcx: &mut FontContext) -> &HashMap<String, Arc<str>> {
        self.squashed.get_or_init(|| fcx.collection.family_names().map(|n| (squash(n), Arc::from(n))).collect())
    }
}

/// Register the font file an environment variable names, returning its
/// family.
fn register_file(collection: &mut Collection, var: &str) -> Option<Arc<str>> {
    let path = std::env::var_os(var)?;
    let bytes = std::fs::read(&path).ok()?;
    let families = collection.register_fonts(Blob::from(bytes), None);
    let (id, _) = families.first()?;
    let name: Arc<str> = collection.family_name(*id)?.into();
    let generics: &[GenericFamily] = if var == "SIDESTEP_MONO_FONT" {
        &[GenericFamily::Monospace]
    } else {
        &[GenericFamily::SansSerif, GenericFamily::SystemUi]
    };
    for &generic in generics {
        let rest: Vec<_> = collection.generic_families(generic).filter(|f| f != id).collect();
        collection.set_generic_families(generic, std::iter::once(*id).chain(rest));
    }
    Some(name)
}

fn squash(name: &str) -> String {
    name.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// Families of Apple's that programs name directly, and what stands in for
/// them.
const APPLE_FAMILIES: &[(&str, Design)] = &[
    ("SF Pro", Design::Default),
    ("SF Pro Text", Design::Default),
    ("SF Pro Display", Design::Default),
    ("SF NS", Design::Default),
    ("San Francisco", Design::Default),
    ("Helvetica", Design::Default),
    ("Helvetica Neue", Design::Default),
    ("Lucida Grande", Design::Default),
    ("Arial", Design::Default),
    ("Geneva", Design::Default),
    ("Avenir", Design::Default),
    ("Avenir Next", Design::Default),
    ("SF Pro Rounded", Design::Rounded),
    ("SF Mono", Design::Monospaced),
    ("Menlo", Design::Monospaced),
    ("Monaco", Design::Monospaced),
    ("Courier", Design::Monospaced),
    ("Courier New", Design::Monospaced),
    ("Andale Mono", Design::Monospaced),
    ("New York", Design::Serif),
    ("Times", Design::Serif),
    ("Times New Roman", Design::Serif),
    ("Georgia", Design::Serif),
    ("Palatino", Design::Serif),
];

/// Style words in font names, as (word, weight, italic, stretch); `None`
/// leaves that attribute alone. Longer words come first so that
/// "semibold" isn't read as "bold".
const STYLE_WORDS: &[(&str, Option<f32>, bool, Option<f32>)] = &[
    ("extralight", Some(200.0), false, None),
    ("ultralight", Some(200.0), false, None),
    ("extrabold", Some(800.0), false, None),
    ("ultrabold", Some(800.0), false, None),
    ("semibold", Some(600.0), false, None),
    ("demibold", Some(600.0), false, None),
    ("semilight", Some(350.0), false, None),
    ("regular", Some(400.0), false, None),
    ("medium", Some(500.0), false, None),
    ("normal", Some(400.0), false, None),
    ("heavy", Some(800.0), false, None),
    ("black", Some(900.0), false, None),
    ("light", Some(300.0), false, None),
    ("thin", Some(100.0), false, None),
    ("bold", Some(700.0), false, None),
    ("book", Some(400.0), false, None),
    ("roman", Some(400.0), false, None),
    ("demi", Some(600.0), false, None),
    ("italic", None, true, None),
    ("oblique", None, true, None),
    ("extracondensed", None, false, Some(0.625)),
    ("semicondensed", None, false, Some(0.875)),
    ("condensed", None, false, Some(0.75)),
    ("semiexpanded", None, false, Some(1.125)),
    ("extraexpanded", None, false, Some(1.5)),
    ("expanded", None, false, Some(1.25)),
];

/// Read style words ("BoldItalic", "Semibold Condensed") into `spec`, or
/// `None` if something in `style` isn't one.
pub(crate) fn parse_style(style: &str, spec: &mut FontSpec) -> Option<()> {
    let mut rest = squash(style);
    rest.retain(|c| c != '-' && c != '_');
    while !rest.is_empty() {
        let &(word, weight, italic, stretch) = STYLE_WORDS.iter().find(|w| rest.starts_with(w.0))?;
        if let Some(w) = weight {
            spec.weight = w;
        }
        spec.italic |= italic;
        if let Some(s) = stretch {
            spec.stretch = s;
        }
        rest.drain(..word.len());
    }
    Some(())
}

/// What `+[NSFont fontWithName:size:]` finds for `name`: a family, a
/// PostScript name (`Family-Style`), a full name (`Family Style`), or one
/// of Apple's families that programs name directly.
pub(crate) fn spec_named(name: &str, size: f64) -> Option<FontSpec> {
    let mut spec = FontSpec::system(Design::Default, size);
    if name.starts_with(".AppleSystemUIFont") || name.starts_with(".SF") {
        if name.contains("Mono") {
            spec.family = Family::System(Design::Monospaced);
        }
        if let Some(style) = name.rsplit_once('-').map(|(_, s)| s) {
            let _ = parse_style(style, &mut spec);
        } else if name.ends_with("Bold") {
            spec.weight = 700.0;
        }
        return Some(spec);
    }
    let shared = shared();
    let found = crate::text::with_ctx(|ctx| {
        let fcx = &mut ctx.fcx;
        if let Some(id) = fcx.collection.family_id(name) {
            return fcx.collection.family_name(id).map(|n| (Arc::<str>::from(n), ""));
        }
        let squashed = shared.squashed(fcx);
        // "Family-Style" and "Family Style", trying longer families first.
        let mut cut = name.len();
        loop {
            let head = &name[..cut];
            if let Some(family) = squashed.get(&squash(head)) {
                return Some((family.clone(), name[cut..].trim_start_matches(['-', ' '])));
            }
            cut = head.rfind(['-', ' '])?;
        }
    });
    if let Some((family, style)) = found {
        spec.family = Family::Named(family);
        parse_style(style, &mut spec)?;
        return Some(spec);
    }
    let (family, style) = name.split_once('-').unwrap_or((name, ""));
    let &(_, design) = APPLE_FAMILIES.iter().find(|(f, _)| f.eq_ignore_ascii_case(family))?;
    spec.family = Family::System(design);
    parse_style(style, &mut spec)?;
    Some(spec)
}

/// The face `spec` resolves to on this thread.
pub(crate) fn resolve(spec: &FontSpec) -> Arc<Face> {
    crate::text::with_ctx(|ctx| {
        let key = spec.key();
        if let Some(face) = ctx.faces.get(&key) {
            return face.clone();
        }
        let face = Arc::new(load_face(&mut ctx.fcx, spec));
        ctx.faces.insert(key, face.clone());
        face
    })
}

fn load_face(fcx: &mut FontContext, spec: &FontSpec) -> Face {
    let family = match &spec.family {
        Family::System(design) => shared().family_for(*design).clone(),
        Family::Named(name) => name.clone(),
    };
    let style = if spec.italic { FontStyle::Italic } else { FontStyle::Normal };
    let attributes = Attributes::new(FontWidth::from_ratio(spec.stretch), style, FontWeight::new(spec.weight));
    let mut found = None;
    let mut query = fcx.collection.query(&mut fcx.source_cache);
    query.set_families([QueryFamily::Named(&family), QueryFamily::Generic(GenericFamily::SansSerif)]);
    query.set_attributes(attributes);
    query.matches_with(|font| {
        found = Some(font.clone());
        QueryStatus::Stop
    });
    drop(query);
    // Parley emboldens a face whenever the weight asked for is above the
    // face's. Browsers only do when bold is asked of a face that isn't, so
    // that Medium in a family without it is Regular, not a smeared Regular;
    // laying out with the face's own weight keeps to that.
    let own_weight = found.as_ref().and_then(|f| {
        let family = fcx.collection.family(f.family.0)?;
        let info = family.fonts().get(f.family.1)?;
        (!info.has_weight_axis()).then(|| info.weight().value())
    });
    let weight = match own_weight {
        Some(w) if !(spec.weight >= 600.0 && w < 600.0) => w,
        _ => spec.weight,
    };
    let matched_family: Arc<str> = found
        .as_ref()
        .and_then(|f| fcx.collection.family_name(f.family.0))
        .map(Arc::from)
        .unwrap_or_else(|| family.clone());
    let mut face = Face {
        family: matched_family.clone(),
        postscript_name: matched_family.clone(),
        full_name: matched_family,
        weight,
        italic: spec.italic,
        stretch: spec.stretch,
        metrics: Metrics {
            ascent: 0.8,
            descent: -0.2,
            cap_height: 0.7,
            x_height: 0.5,
            underline_position: -0.1,
            underline_thickness: 0.05,
            strikeout_position: 0.3,
            strikeout_thickness: 0.05,
            bounds: [0.0, -0.2, 1.0, 0.8],
            max_advance: 1.0,
            ..Metrics::default()
        },
        fixed_pitch: false,
        glyph_count: 0,
        font: None,
        variations: Vec::new(),
    };
    if let Some(font) = found {
        face.variations = font.synthesis.variation_settings().iter().map(|&(tag, value)| (tag, value)).collect();
        describe(&mut face, font.blob.as_ref(), font.index);
        face.font = Some(FontData::new(font.blob, font.index));
    }
    face
}

/// A glyph's advance and bounding box (x0, y0, x1, y1, y up), per point.
pub(crate) fn glyph_metrics(face: &Face, glyph: u32) -> Option<(f32, [f32; 4])> {
    let data = face.font.as_ref()?;
    let font = skrifa::FontRef::from_index(data.data.data(), data.index).ok()?;
    let location = font.axes().location(face.variations.iter().copied());
    let upem = f32::from(font.metrics(Size::unscaled(), &location).units_per_em.max(1));
    let metrics = font.glyph_metrics(Size::unscaled(), &location);
    let id = skrifa::GlyphId::new(glyph);
    let advance = metrics.advance_width(id)?;
    let b = metrics.bounds(id).unwrap_or_default();
    Some((advance / upem, [b.x_min / upem, b.y_min / upem, b.x_max / upem, b.y_max / upem]))
}

/// Fill in `face`'s names and metrics from its font file.
fn describe(face: &mut Face, data: &[u8], index: u32) {
    let Ok(font) = skrifa::FontRef::from_index(data, index) else { return };
    let location: Location = font.axes().location(face.variations.iter().copied());
    let m = font.metrics(Size::unscaled(), &location);
    let upem = f32::from(m.units_per_em.max(1));
    let name = |id: StringId| -> Option<Arc<str>> {
        let s: String = font.localized_strings(id).english_or_first()?.chars().collect();
        (!s.is_empty()).then(|| s.into())
    };
    if let Some(n) = name(StringId::POSTSCRIPT_NAME) {
        face.postscript_name = n;
    }
    if let Some(n) = name(StringId::FULL_NAME) {
        face.full_name = n;
    }
    let bounds = m.bounds.map(|b| [b.x_min, b.y_min, b.x_max, b.y_max]).unwrap_or([0.0, m.descent, upem, m.ascent]);
    face.metrics = Metrics {
        ascent: m.ascent / upem,
        descent: m.descent / upem,
        leading: m.leading / upem,
        cap_height: m.cap_height.unwrap_or(m.ascent * 0.7) / upem,
        x_height: m.x_height.unwrap_or(m.ascent * 0.5) / upem,
        underline_position: m.underline.map_or(-upem / 10.0, |u| u.offset) / upem,
        underline_thickness: m.underline.map_or(upem / 18.0, |u| u.thickness) / upem,
        // HarfBuzz's defaults, as parley's.
        strikeout_position: m.strikeout.map_or(m.ascent / 2.0, |s| s.offset) / upem,
        strikeout_thickness: m.strikeout.map_or(upem / 18.0, |s| s.thickness) / upem,
        bounds: bounds.map(|v| v / upem),
        italic_angle: m.italic_angle,
        max_advance: m.max_width.unwrap_or(bounds[2] - bounds[0]) / upem,
    };
    face.glyph_count = u32::from(m.glyph_count);
    face.fixed_pitch = m.is_monospace || {
        // Not every monospaced font says so; compare some advances.
        let charmap = font.charmap();
        let widths = font.glyph_metrics(Size::unscaled(), &location);
        let advance = |c: char| charmap.map(c).and_then(|g| widths.advance_width(g));
        match (advance('i'), advance('W'), advance('0')) {
            (Some(a), Some(b), Some(c)) => a == b && b == c && a > 0.0,
            _ => false,
        }
    };
}

/// A face as the render thread draws it: the font file, its variation
/// coordinates, and how its glyphs are drawn beyond their outlines.
#[derive(Clone)]
pub(crate) struct FaceData {
    pub font: FontData,
    pub coords: Arc<[i16]>,
    pub embolden: bool,
    /// Degrees to slant by: a synthesized oblique and `NSObliqueness`.
    pub skew: f32,
    /// The width of a stroke along the outlines, as a fraction of the
    /// size, for `NSStrokeWidth`; 0 fills them.
    pub stroke: f32,
}

/// What drawing a face adds to its outlines: bold and slant that are
/// synthesized, and a stroke along them in place of a fill.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Synth {
    pub embolden: bool,
    /// Degrees, right for positive values.
    pub skew: f32,
    /// A fraction of the size; 0 fills the glyphs.
    pub stroke: f32,
}

#[derive(Default)]
struct Registry {
    faces: Vec<FaceData>,
    /// Candidates by font file, index and synthesis; coordinates are
    /// compared on lookup.
    ids: HashMap<(u64, u32, bool, u32, u32), Vec<u32>>,
}

static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(Default::default);

/// The id glyph runs name a face by. Faces are never unregistered: the
/// shared source cache hands out the same font data for a file as long as
/// someone holds it, which the registry does, so ids stay few and stable.
pub(crate) fn register(font: &FontData, coords: &[i16], synth: Synth) -> u32 {
    // Adding zero makes -0 and 0 one key.
    let key =
        (font.data.id(), font.index, synth.embolden, (synth.skew + 0.0).to_bits(), (synth.stroke + 0.0).to_bits());
    let find =
        |reg: &Registry| reg.ids.get(&key)?.iter().copied().find(|&id| *reg.faces[id as usize].coords == *coords);
    if let Some(id) = find(&REGISTRY.read().unwrap_or_else(|e| e.into_inner())) {
        return id;
    }
    let mut reg = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    if let Some(id) = find(&reg) {
        return id;
    }
    let id = reg.faces.len() as u32;
    let (embolden, skew, stroke) = (synth.embolden, synth.skew, synth.stroke);
    reg.faces.push(FaceData { font: font.clone(), coords: coords.into(), embolden, skew, stroke });
    reg.ids.entry(key).or_default().push(id);
    id
}

/// The face registered as `id`.
pub(crate) fn face_data(id: u32) -> Option<FaceData> {
    REGISTRY.read().unwrap_or_else(|e| e.into_inner()).faces.get(id as usize).cloned()
}

/// `NSFontWeight` (−1 to 1) as a CSS weight, through the named weights,
/// whose values are single precision as AppKit's are.
pub(crate) fn css_weight(ns: f64) -> f32 {
    const STOPS: [(f64, f64); 9] = [
        (-0.8f32 as f64, 100.0),
        (-0.6f32 as f64, 200.0),
        (-0.4f32 as f64, 300.0),
        (0.0, 400.0),
        (0.23f32 as f64, 500.0),
        (0.3f32 as f64, 600.0),
        (0.4f32 as f64, 700.0),
        (0.56f32 as f64, 800.0),
        (0.62f32 as f64, 900.0),
    ];
    if ns <= STOPS[0].0 {
        return (100.0 + (ns - STOPS[0].0) * 500.0).max(1.0) as f32;
    }
    for pair in STOPS.windows(2) {
        let ((a, wa), (b, wb)) = (pair[0], pair[1]);
        if ns <= b {
            return (wa + (ns - a) / (b - a) * (wb - wa)) as f32;
        }
    }
    (900.0 + (ns - STOPS[8].0) * 263.0).min(1000.0) as f32
}
