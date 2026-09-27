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
use skrifa::raw::TableProvider;
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
    /// A face from a font file a program loaded itself (a `CGFont` made
    /// into a CoreText font): see [`register_data`].
    Data(DataFamily),
}

/// A font file's face, laid out under a private family name of its own
/// (registered in the font collection when a font is first made of it),
/// so that text laid out in it finds exactly it, whatever families the
/// system has.
#[derive(Clone, Debug)]
pub(crate) struct DataFamily(pub Arc<DataFace>);

#[derive(Debug)]
pub(crate) struct DataFace {
    /// The private family name it's laid out under.
    pub family: Arc<str>,
    /// Its own weight, style and width, which a spec asks for so that
    /// nothing is synthesized.
    pub weight: f32,
    pub italic: bool,
    pub stretch: f32,
    /// The face's file.
    pub font: FontData,
    /// Whether the collection took it (it may not: a face without a
    /// character map can't lay text out), once asked.
    in_collection: OnceLock<bool>,
    /// Names the face.
    id: u64,
}

impl DataFace {
    /// Register the face in `fcx`'s collection the first time; whether
    /// the collection has it.
    fn collected(&self, fcx: &mut FontContext) -> bool {
        *self.in_collection.get_or_init(|| {
            let over = parley::fontique::FontInfoOverride { family_name: Some(&self.family), ..Default::default() };
            let registered = fcx.collection.register_fonts(self.font.data.clone(), Some(over));
            registered.iter().flat_map(|(_, fonts)| fonts).any(|f| f.index() == self.font.index)
        })
    }
}

impl PartialEq for DataFamily {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}

impl Eq for DataFamily {}

impl std::hash::Hash for DataFamily {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.id.hash(state);
    }
}

/// The prefix of the private family names font files are laid out under,
/// which lists of the system's families leave out.
pub(crate) const PRIVATE_FAMILY: &str = ".SidestepFont-";

#[derive(Default)]
struct DataRegistry {
    /// By blob id and index, and by the file's contents (a hash, the
    /// length and the index, then the bytes compared): a program that
    /// loads the same file again gets the face it got the first time.
    by_blob: HashMap<(u64, u32), DataFamily>,
    by_content: HashMap<(u64, usize, u32), Vec<DataFamily>>,
    /// Faces registered by name (`CTFontManagerRegisterGraphicsFont`), by
    /// PostScript name and full name.
    named: HashMap<String, DataFamily>,
}

static DATA: LazyLock<Mutex<DataRegistry>> = LazyLock::new(Default::default);

/// Blobs remembered before the blob map starts over (the content map
/// still finds their faces).
const BLOBS: usize = 1024;

fn content_key(font: &FontData) -> (u64, usize, u32) {
    let bytes = font.data.data();
    let mut h = super::layout::Fx::default();
    std::hash::Hasher::write(&mut h, bytes);
    (std::hash::Hasher::finish(&h), bytes.len(), font.index)
}

impl DataRegistry {
    fn find(&mut self, font: &FontData, content: Option<(u64, usize, u32)>) -> Option<DataFamily> {
        let blob_key = (font.data.id(), font.index);
        if let Some(found) = self.by_blob.get(&blob_key) {
            return Some(found.clone());
        }
        let bytes = font.data.data();
        let found = self
            .by_content
            .get(&content?)?
            .iter()
            .find(|f| f.0.font.data.id() == font.data.id() || f.0.font.data.data() == bytes)
            .cloned()?;
        self.remember_blob(blob_key, &found);
        Some(found)
    }

    fn remember_blob(&mut self, key: (u64, u32), family: &DataFamily) {
        if self.by_blob.len() >= BLOBS {
            self.by_blob.clear();
        }
        self.by_blob.insert(key, family.clone());
    }
}

/// The face made of `font` before, if any: nothing new is made.
pub(crate) fn data_face(font: &FontData) -> Option<DataFamily> {
    let mut reg = DATA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = reg.find(font, None) {
        return Some(found);
    }
    drop(reg);
    let content = content_key(font);
    DATA.lock().unwrap_or_else(|e| e.into_inner()).find(font, Some(content))
}

/// The face `font` is, made the first time (a file makes one face however
/// often it's loaded, so a program holds a bounded number of them, which
/// are never let go). `None` if it isn't a font.
pub(crate) fn register_data(font: &FontData) -> Option<DataFamily> {
    if let Some(found) = DATA.lock().unwrap_or_else(|e| e.into_inner()).find(font, None) {
        return Some(found);
    }
    let made = new_data_face(font)?;
    // Hashing a large file takes a while: not while holding the lock.
    let content = content_key(font);
    let mut reg = DATA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = reg.find(font, Some(content)) {
        return Some(found);
    }
    reg.by_content.entry(content).or_default().push(made.clone());
    reg.remember_blob((font.data.id(), font.index), &made);
    Some(made)
}

/// A face of `font` not remembered anywhere (for a descriptor of a file
/// that may never be used), unless one was made before.
pub(crate) fn peek_data(font: &FontData) -> Option<DataFamily> {
    data_face(font).or_else(|| new_data_face(font))
}

fn new_data_face(font: &FontData) -> Option<DataFamily> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let file = skrifa::FontRef::from_index(font.data.data(), font.index).ok()?;
    let a = file.attributes();
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let face = DataFace {
        family: format!("{PRIVATE_FAMILY}{id}").into(),
        weight: a.weight.value(),
        italic: a.style != skrifa::attribute::Style::Normal,
        stretch: a.stretch.ratio(),
        font: font.clone(),
        in_collection: OnceLock::new(),
        id,
    };
    Some(DataFamily(Arc::new(face)))
}

/// A name from `font`'s `name` table: the English one, or the first there
/// is; none if it's missing or empty.
pub(crate) fn name_of(font: &skrifa::FontRef<'_>, id: StringId) -> Option<String> {
    let s: String = font.localized_strings(id).english_or_first()?.chars().collect();
    (!s.is_empty()).then_some(s)
}

/// A file face's name from its `name` table (`id`), or its private
/// family's.
fn data_string(face: &DataFace, id: StringId) -> String {
    skrifa::FontRef::from_index(face.font.data.data(), face.font.index)
        .ok()
        .and_then(|f| name_of(&f, id))
        .unwrap_or_else(|| face.family.to_string())
}

/// A file face's PostScript name.
pub(crate) fn data_name(face: &DataFace) -> String {
    data_string(face, StringId::POSTSCRIPT_NAME)
}

/// A file face's full name.
pub(crate) fn data_full_name(face: &DataFace) -> String {
    data_string(face, StringId::FULL_NAME)
}

/// The system's family names, in order (case aside), leaving out the
/// private ones font files are laid out under.
pub(crate) fn family_names() -> Vec<String> {
    let mut names: Vec<String> = crate::text::with_ctx(|ctx| {
        ctx.fcx.collection.family_names().filter(|n| !n.starts_with(PRIVATE_FAMILY)).map(String::from).collect()
    });
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// The weight, style and width of each face of the family `name`.
pub(crate) fn family_faces(name: &str) -> Vec<(f32, bool, f32)> {
    crate::text::with_ctx(|ctx| {
        let Some(family) = ctx.fcx.collection.family_by_name(name) else { return Vec::new() };
        family
            .fonts()
            .iter()
            .map(|f| (f.weight().value(), !matches!(f.style(), FontStyle::Normal), f.width().ratio()))
            .collect()
    })
}

/// Make `family` findable by its PostScript and full names, as a font
/// registered with the font manager is.
pub(crate) fn register_named(family: &DataFamily, names: &[&str]) {
    let mut reg = DATA.lock().unwrap_or_else(|e| e.into_inner());
    for name in names.iter().filter(|n| !n.is_empty()) {
        reg.named.insert((*name).to_string(), family.clone());
    }
}

/// Forget the names [`register_named`] gave `family`; whether it had any.
pub(crate) fn unregister_named(family: &DataFamily) -> bool {
    let mut reg = DATA.lock().unwrap_or_else(|e| e.into_inner());
    let before = reg.named.len();
    reg.named.retain(|_, f| f != family);
    reg.named.len() != before
}

/// The names faces were registered by with [`register_named`].
pub(crate) fn registered_names() -> Vec<(String, DataFamily)> {
    let reg = DATA.lock().unwrap_or_else(|e| e.into_inner());
    reg.named.iter().map(|(n, f)| (n.clone(), f.clone())).collect()
}

impl FontSpec {
    /// A spec for a registered file's face, as it is (nothing synthesized).
    pub fn data(family: DataFamily, size: f64) -> Self {
        let (weight, italic, stretch) = (family.0.weight, family.0.italic, family.0.stretch);
        FontSpec { family: Family::Data(family), weight, italic, stretch, ..FontSpec::system(Design::Default, size) }
    }
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
    /// Variation axis values a descriptor set (tag and value), which the
    /// face is matched and laid out at.
    pub variations: Option<Variations>,
    /// The spec names a family or font the system doesn't have, so no font
    /// matches it (`fontWithDescriptor:size:` gives nil); `family` holds
    /// the name.
    pub missing: bool,
}

/// OpenType feature tags and values.
pub(crate) type Features = Arc<[([u8; 4], u16)]>;

/// Variation axis tags and values.
pub(crate) type Variations = Arc<[([u8; 4], f32)]>;

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
            variations: None,
            missing: false,
        }
    }

    pub(crate) fn key(&self) -> FaceKey {
        FaceKey {
            family: self.family.clone(),
            weight: self.weight.to_bits(),
            italic: self.italic,
            stretch: self.stretch.to_bits(),
            variations: self.variations.as_ref().map(|v| v.iter().map(|&(t, x)| (t, x.to_bits())).collect()),
        }
    }
}

/// Variation tags and the bits of their values, as a key.
type VariationBits = Arc<[([u8; 4], u32)]>;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct FaceKey {
    family: Family,
    weight: u32,
    italic: bool,
    stretch: u32,
    variations: Option<VariationBits>,
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

/// A face's metrics in its own units, as CoreText reports them scaled
/// to a size (exactly: the file's integers times the size over the units
/// per em). Distances follow [`Metrics`]' signs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Units {
    pub per_em: f64,
    pub ascent: f64,
    pub descent: f64,
    pub leading: f64,
    pub cap_height: f64,
    pub x_height: f64,
    pub underline_position: f64,
    pub underline_thickness: f64,
    /// x0, y0, x1, y1.
    pub bounds: [f64; 4],
    pub italic_angle: f64,
    /// The typographic ascender and descender (`OS/2`), for vertical
    /// glyph metrics.
    pub typo_ascender: f64,
    pub typo_descender: f64,
}

/// A face resolved from a [`FontSpec`], with what `NSFont` reports about it.
#[derive(Debug)]
pub(crate) struct Face {
    /// The family to lay text out in, as the collection names it.
    pub family: Arc<str>,
    /// The family name to report: the collection's, or a font file's own
    /// (from its `name` table) for a face registered under a private one.
    pub family_name: Arc<str>,
    pub postscript_name: Arc<str>,
    pub full_name: Arc<str>,
    /// What layout asks for to get this face: the spec's width and style,
    /// and the face's own weight unless it needs emboldening (see
    /// [`load_face`]).
    pub weight: f32,
    pub italic: bool,
    pub stretch: f32,
    pub metrics: Metrics,
    pub units: Units,
    pub fixed_pitch: bool,
    pub glyph_count: u32,
    /// The font file, and the variation settings the face was matched with
    /// (and those a descriptor set, which follow them).
    pub font: Option<FontData>,
    pub variations: Vec<(skrifa::Tag, f32)>,
    /// The variation settings a descriptor set, which layout asks for.
    pub set_variations: Vec<(skrifa::Tag, f32)>,
    /// Its glyphs' outline boxes, as they're asked for.
    pub boxes: GlyphBoxes,
    /// Its glyphs by name (from `post` or the CFF charset), made the first
    /// time one is looked up.
    pub glyph_names: OnceLock<HashMap<Box<str>, u16>>,
}

/// A face's glyphs' outline boxes (x0, y0, x1, y1 in font units; none for
/// an empty outline), found once each: finding one can mean drawing the
/// outline. At most one entry a glyph.
#[derive(Debug, Default)]
pub(crate) struct GlyphBoxes(RwLock<HashMap<u16, Option<[f32; 4]>>>);

impl GlyphBoxes {
    /// Call `f` with each of `glyphs`' positions and its box if known.
    pub fn lookup(&self, glyphs: &[u16], mut f: impl FnMut(usize, Option<Option<[f32; 4]>>)) {
        let known = self.0.read().unwrap_or_else(|e| e.into_inner());
        for (k, g) in glyphs.iter().enumerate() {
            f(k, known.get(g).copied());
        }
    }

    pub fn remember(&self, found: impl Iterator<Item = (u16, Option<[f32; 4]>)>) {
        let mut known = self.0.write().unwrap_or_else(|e| e.into_inner());
        known.extend(found);
    }
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
    if let Some(family) = DATA.lock().unwrap_or_else(|e| e.into_inner()).named.get(name).cloned() {
        return Some(FontSpec::data(family, size));
    }
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
        // Weights and variation values vary continuously (an animation, a
        // slider): the faces a thread remembers start over past a bound,
        // as `NSFont`'s cache does.
        if ctx.faces.len() >= FACES {
            ctx.faces.clear();
        }
        ctx.faces.insert(key, face.clone());
        face
    })
}

/// Faces a thread remembers before it starts over.
const FACES: usize = 512;

fn load_face(fcx: &mut FontContext, spec: &FontSpec) -> Face {
    let family = match &spec.family {
        Family::System(design) => shared().family_for(*design).clone(),
        Family::Named(name) => name.clone(),
        Family::Data(data) if !data.0.collected(fcx) => return file_face(&data.0, spec),
        Family::Data(data) => data.0.family.clone(),
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
    let mut face = Face::unfound(matched_family, weight, spec);
    if let Some(font) = found {
        face.variations = font.synthesis.variation_settings().iter().map(|&(tag, value)| (tag, value)).collect();
        face.set_variations(spec, &font.blob, font.index);
        describe(&mut face, font.blob.as_ref(), font.index);
        face.font = Some(FontData::new(font.blob, font.index));
    }
    face
}

/// The face of a font file the collection wouldn't take (one without a
/// character map): its metrics and glyphs are there, but text laid out in
/// it falls back on the system's faces.
fn file_face(data: &DataFace, spec: &FontSpec) -> Face {
    let mut face = Face::unfound(data.family.clone(), data.weight, spec);
    face.set_variations(spec, &data.font.data, data.font.index);
    describe(&mut face, data.font.data.data(), data.font.index);
    face.font = Some(data.font.clone());
    face
}

impl Face {
    /// A face of `family` with the metrics of none, before a file
    /// describes it.
    fn unfound(family: Arc<str>, weight: f32, spec: &FontSpec) -> Face {
        Face {
            family: family.clone(),
            family_name: family.clone(),
            postscript_name: family.clone(),
            full_name: family,
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
            units: Units::default(),
            fixed_pitch: false,
            glyph_count: 0,
            font: None,
            variations: Vec::new(),
            set_variations: Vec::new(),
            boxes: GlyphBoxes::default(),
            glyph_names: OnceLock::new(),
        }
    }

    /// What a descriptor set, over what matching chose, for the axes the
    /// font has.
    fn set_variations(&mut self, spec: &FontSpec, blob: &parley::fontique::Blob<u8>, index: u32) {
        let Some(set) = &spec.variations else { return };
        let Ok(file) = skrifa::FontRef::from_index(blob.as_ref(), index) else { return };
        let axes = file.axes();
        for &(tag, value) in set.iter() {
            let tag = skrifa::Tag::new(&tag);
            if axes.iter().any(|a| a.tag() == tag) {
                self.variations.retain(|v| v.0 != tag);
                self.variations.push((tag, value));
                self.set_variations.push((tag, value));
            }
        }
    }
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

/// A face's cap height and x-height in font units, at `location`:
/// `OS/2`'s, or without them (tables before version 2) where CoreText
/// finds them, halfway between a flat letter's top and a round one's
/// (H's and O's, x's and o's), rounded down; measured on macOS, which
/// overshoots with the round letter. Fonts without the letters get
/// shares of the ascent.
pub(crate) fn heights(font: &skrifa::FontRef<'_>, location: &Location) -> (f32, f32) {
    let m = font.metrics(Size::unscaled(), location);
    let tops = font.glyph_metrics(Size::unscaled(), location);
    let charmap = font.charmap();
    let top = |c: char| charmap.map(c).and_then(|g| tops.bounds(g)).map(|b| b.y_max);
    let height = |given: Option<f32>, flat: char, round: char, share: f32| match (given, top(flat), top(round)) {
        (Some(h), ..) if h > 0.0 => h,
        (_, Some(a), Some(b)) => ((a + b) / 2.0).floor(),
        (_, Some(a), None) => a,
        _ => m.ascent * share,
    };
    (height(m.cap_height, 'H', 'O', 0.7), height(m.x_height, 'x', 'o', 0.5))
}

/// Fill in `face`'s names and metrics from its font file.
fn describe(face: &mut Face, data: &[u8], index: u32) {
    let Ok(font) = skrifa::FontRef::from_index(data, index) else { return };
    let location: Location = font.axes().location(face.variations.iter().copied());
    let m = font.metrics(Size::unscaled(), &location);
    let upem = f32::from(m.units_per_em.max(1));
    let name = |id: StringId| -> Option<Arc<str>> { name_of(&font, id).map(Arc::from) };
    if let Some(n) = name(StringId::POSTSCRIPT_NAME) {
        face.postscript_name = n;
    }
    if let Some(n) = name(StringId::FULL_NAME) {
        face.full_name = n;
    }
    // A file registered under a private family reports its own.
    if face.family.starts_with(PRIVATE_FAMILY)
        && let Some(n) = name(StringId::FAMILY_NAME)
    {
        face.family_name = n;
    }
    let bounds = m.bounds.map(|b| [b.x_min, b.y_min, b.x_max, b.y_max]).unwrap_or([0.0, m.descent, upem, m.ascent]);
    let (cap_height, x_height) = heights(&font, &location);
    let (typo_ascender, typo_descender) = match font.os2() {
        Ok(os2) => (f64::from(os2.s_typo_ascender()), f64::from(os2.s_typo_descender())),
        Err(_) => (f64::from(m.ascent), f64::from(m.descent)),
    };
    face.units = Units {
        per_em: f64::from(upem),
        ascent: f64::from(m.ascent),
        descent: f64::from(m.descent),
        leading: f64::from(m.leading),
        cap_height: f64::from(cap_height),
        x_height: f64::from(x_height),
        underline_position: m.underline.map_or(-f64::from(upem) / 10.0, |u| f64::from(u.offset)),
        underline_thickness: m.underline.map_or(f64::from(upem) / 18.0, |u| f64::from(u.thickness)),
        bounds: bounds.map(f64::from),
        italic_angle: f64::from(m.italic_angle),
        typo_ascender,
        typo_descender,
    };
    face.metrics = Metrics {
        ascent: m.ascent / upem,
        descent: m.descent / upem,
        leading: m.leading / upem,
        cap_height: cap_height / upem,
        x_height: x_height / upem,
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

impl Synth {
    /// The slant in tenths of a degree, up to 89 either way, and the
    /// stroke in thousandths of the size, up to the whole size (a thinner
    /// one stays a hairline rather than becoming a fill): an app that
    /// animates `NSObliqueness` or `NSStrokeWidth`, or binds it to a
    /// slider, gets a face per step rather than one per value.
    pub fn stepped(self) -> Synth {
        let real = |v: f32| if v.is_finite() { v } else { 0.0 };
        let skew = (real(self.skew).clamp(-89.0, 89.0) * 10.0).round() / 10.0;
        let stroke = real(self.stroke).clamp(0.0, 1.0);
        let stroke = if stroke > 0.0 { ((stroke * 1000.0).round() / 1000.0).max(0.001) } else { 0.0 };
        // Adding zero makes -0 and 0 one key.
        Synth { embolden: self.embolden, skew: skew + 0.0, stroke }
    }
}

/// A face's file, index and synthesis, as a key.
type FileKey = (u64, u32, bool, u32, u32);

#[derive(Default)]
struct Registry {
    faces: Vec<FaceData>,
    /// Ids by font file, index and synthesis, then by coordinates.
    ids: HashMap<FileKey, HashMap<Box<[i16]>, u32>>,
}

static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(Default::default);

/// The id glyph runs name a face by. Faces are never unregistered: the
/// shared source cache hands out the same font data for a file as long as
/// someone holds it, which the registry does, and slants and strokes are
/// kept in steps too fine to see ([`Synth::stepped`]), so a font has a
/// bounded number of faces however an app varies them, and ids are stable.
pub(crate) fn register(font: &FontData, coords: &[i16], synth: Synth) -> u32 {
    let synth = synth.stepped();
    let key = (font.data.id(), font.index, synth.embolden, synth.skew.to_bits(), synth.stroke.to_bits());
    let find = |reg: &Registry| reg.ids.get(&key)?.get(coords).copied();
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
    reg.ids.entry(key).or_default().insert(coords.into(), id);
    id
}

/// The face registered as `id`.
pub(crate) fn face_data(id: u32) -> Option<FaceData> {
    REGISTRY.read().unwrap_or_else(|e| e.into_inner()).faces.get(id as usize).cloned()
}

/// `NSFontWeight` (−1 to 1) as a CSS weight, through the named weights,
/// whose values are single precision as AppKit's are.
pub(crate) fn css_weight(ns: f64) -> f32 {
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

/// A CSS weight as `NSFontWeight`, the inverse of [`css_weight`].
pub(crate) fn ns_weight(css: f32) -> f64 {
    let css = f64::from(css);
    if css <= STOPS[0].1 {
        return STOPS[0].0 + (css - 100.0) / 500.0;
    }
    for pair in STOPS.windows(2) {
        let ((a, wa), (b, wb)) = (pair[0], pair[1]);
        if css <= wb {
            return a + (css - wa) / (wb - wa) * (b - a);
        }
    }
    STOPS[8].0 + (css - 900.0) / 263.0
}

/// The named weights: `NSFontWeight` values (single precision, as
/// AppKit's are) and CSS weights.
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
