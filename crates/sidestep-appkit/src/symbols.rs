//! Symbol images: `+[NSImage imageWithSystemSymbolName:accessibilityDescription:]`
//! and `NSImageSymbolConfiguration`.
//!
//! macOS draws symbols from its SF Symbols font, which Sidestep can't
//! ship. Instead a common set of symbol names maps to Sidestep's own line
//! drawings, on a 24-unit grid in the style of the open icon sets
//! (strokes two units wide, rounded), stored below as SVG path data. A
//! name without a drawing gives nil, as an unknown name does on macOS.
//!
//! A symbol image is a template image drawn by a drawing handler, black
//! (views tint templates), about 13 points tall at 13 points (the size of
//! the body text) and as wide as its drawing. A configuration's point
//! size (or text style) and scale grow it; its weight thickens the lines.

use std::sync::OnceLock;

use block2::RcBlock;
use kurbo::{BezPath, Point, Shape};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_app_kit::{
    NSColor, NSCompositingOperation, NSFont, NSFontTextStyle, NSGraphicsContext, NSImage, NSImageSymbolConfiguration,
    NSImageSymbolScale, NSLineCapStyle, NSLineJoinStyle,
};
use objc2_foundation::{NSArray, NSCopying, NSDictionary, NSRect, NSSize, NSString, NSZone};

sidestep_runtime::static_class!(
    pub NSIMAGESYMBOLCONFIGURATION,
    NSIMAGESYMBOLCONFIGURATION_META = "NSImageSymbolConfiguration",
    || {
        let _ = NSImageSymbolConfigurationImpl::class();
    }
);

/// How a part of a drawing is inked.
#[derive(Clone, Copy, PartialEq)]
enum Ink {
    Stroke,
    Fill,
    /// Stroked, taking away what's under it (a mark knocked out of a
    /// filled circle).
    Erase,
    /// Filled, taking away what's under it.
    EraseFill,
}

use Ink::{Erase, EraseFill, Fill, Stroke};

/// Path data for the parts that recur.
const CIRCLE: &str = "M21 12a9 9 0 1 1-18 0a9 9 0 1 1 18 0Z";
/// No path data: the gear is drawn in code ([`gear`]).
const GEAR: &str = "";

/// Sidestep's drawings, by symbol name.
static SYMBOLS: &[(&str, &[(Ink, &str)])] = &[
    ("plus", &[(Stroke, "M12 5v14M5 12h14")]),
    ("minus", &[(Stroke, "M5 12h14")]),
    ("xmark", &[(Stroke, "M6 6l12 12M18 6L6 18")]),
    ("checkmark", &[(Stroke, "M4.5 12.5l5 5L19.5 6.5")]),
    ("chevron.left", &[(Stroke, "M15 5l-7 7 7 7")]),
    ("chevron.right", &[(Stroke, "M9 5l7 7-7 7")]),
    ("chevron.up", &[(Stroke, "M5 15l7-7 7 7")]),
    ("chevron.down", &[(Stroke, "M5 9l7 7 7-7")]),
    ("chevron.up.chevron.down", &[(Stroke, "M7 9l5-5 5 5M7 15l5 5 5-5")]),
    ("arrow.left", &[(Stroke, "M20 12H4M10 6l-6 6 6 6")]),
    ("arrow.right", &[(Stroke, "M4 12h16M14 6l6 6-6 6")]),
    ("arrow.up", &[(Stroke, "M12 20V4M6 10l6-6 6 6")]),
    ("arrow.down", &[(Stroke, "M12 4v16M6 14l6 6 6-6")]),
    ("arrow.up.arrow.down", &[(Stroke, "M8 20V5M4.5 8.5L8 5l3.5 3.5M16 4v15M12.5 15.5L16 19l3.5-3.5")]),
    ("arrow.down.to.line", &[(Stroke, "M12 4v11M7 10l5 5 5-5M5 20h14")]),
    ("arrow.clockwise", &[(Stroke, "M19 12a7 7 0 1 1-2.05-4.95M19 4v4h-4")]),
    ("arrow.counterclockwise", &[(Stroke, "M5 12a7 7 0 1 0 2.05-4.95M5 4v4h4")]),
    ("magnifyingglass", &[(Stroke, "M17 10.5a6.5 6.5 0 1 1-13 0a6.5 6.5 0 1 1 13 0ZM15.5 15.5L20 20")]),
    ("star", &[(Stroke, STAR)]),
    ("star.fill", &[(Fill, STAR)]),
    ("heart", &[(Stroke, HEART)]),
    ("heart.fill", &[(Fill, HEART)]),
    ("circle", &[(Stroke, CIRCLE)]),
    ("circle.fill", &[(Fill, CIRCLE)]),
    ("square", &[(Stroke, SQUARE)]),
    ("square.fill", &[(Fill, SQUARE)]),
    ("info.circle", &[(Stroke, CIRCLE), (Stroke, "M12 11v6"), (Fill, "M13 7.5a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z")]),
    (
        "exclamationmark.circle",
        &[(Stroke, CIRCLE), (Stroke, "M12 7v6"), (Fill, "M13 16.5a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z")],
    ),
    (
        "questionmark.circle",
        &[
            (Stroke, CIRCLE),
            (Stroke, "M9.5 9.5a2.5 2.5 0 1 1 3.5 2.3c-.7.3-1 .9-1 1.7"),
            (Fill, "M13 17a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z"),
        ],
    ),
    (
        "exclamationmark.triangle",
        &[(Stroke, "M12 3.5L21.5 20h-19Z"), (Stroke, "M12 9.5v4.5"), (Fill, "M13 17a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z")],
    ),
    ("checkmark.circle", &[(Stroke, CIRCLE), (Stroke, "M8 12.5l3 3 5-6")]),
    ("checkmark.circle.fill", &[(Fill, CIRCLE), (Erase, "M8 12.5l3 3 5-6")]),
    ("xmark.circle", &[(Stroke, CIRCLE), (Stroke, "M9 9l6 6M15 9l-6 6")]),
    ("xmark.circle.fill", &[(Fill, CIRCLE), (Erase, "M9 9l6 6M15 9l-6 6")]),
    ("plus.circle", &[(Stroke, CIRCLE), (Stroke, "M12 8v8M8 12h8")]),
    ("plus.circle.fill", &[(Fill, CIRCLE), (Erase, "M12 8v8M8 12h8")]),
    ("minus.circle", &[(Stroke, CIRCLE), (Stroke, "M8 12h8")]),
    ("arrow.up.circle", &[(Stroke, CIRCLE), (Stroke, "M12 16.5v-9M8 11l4-4 4 4")]),
    ("arrow.up.circle.fill", &[(Fill, CIRCLE), (Erase, "M12 16.5v-9M8 11l4-4 4 4")]),
    ("arrow.down.circle", &[(Stroke, CIRCLE), (Stroke, "M12 7.5v9M8 13l4 4 4-4")]),
    ("arrow.down.circle.fill", &[(Fill, CIRCLE), (Erase, "M12 7.5v9M8 13l4 4 4-4")]),
    ("stop.circle.fill", &[(Fill, CIRCLE), (EraseFill, "M9 9h6v6H9Z")]),
    ("ellipsis.circle", &[(Stroke, CIRCLE), (Fill, DOTS_SMALL)]),
    ("clock", &[(Stroke, CIRCLE), (Stroke, "M12 7v5l3.5 2")]),
    (
        "globe",
        &[(Stroke, CIRCLE), (Stroke, "M3 12h18M12 3c3 3 3.5 6 3.5 9s-.5 6-3.5 9M12 3c-3 3-3.5 6-3.5 9s.5 6 3.5 9")],
    ),
    ("play", &[(Stroke, PLAY)]),
    ("play.fill", &[(Fill, PLAY)]),
    ("pause.fill", &[(Fill, "M6 4h4v16H6ZM14 4h4v16h-4Z")]),
    ("stop.fill", &[(Fill, "M5 5h14v14H5Z")]),
    ("trash", &[(Stroke, "M4 6.5h16M9.5 6.5V4h5v2.5M6 6.5l1 13.5h10l1-13.5M10 10.5v6M14 10.5v6")]),
    ("gearshape", &[(Stroke, GEAR)]),
    ("gear", &[(Stroke, GEAR)]),
    ("gearshape.fill", &[(Fill, GEAR), (EraseFill, "M15 12a3 3 0 1 1-6 0a3 3 0 1 1 6 0Z")]),
    ("folder", &[(Stroke, FOLDER)]),
    ("folder.fill", &[(Fill, FOLDER)]),
    ("doc", &[(Stroke, "M6 3h8l4 4v14H6ZM14 3v4h4")]),
    ("doc.fill", &[(Fill, "M6 3h8l4 4v14H6Z")]),
    ("doc.on.doc", &[(Stroke, "M9 3h7l4 4v11h-3M4 7h8l4 4v10H4Z")]),
    ("square.and.arrow.up", &[(Stroke, "M12 3v12M8 7l4-4 4 4M8 10H5v10h14V10h-3")]),
    ("square.and.pencil", &[(Stroke, "M11 4H5v15h15v-6M18 3l3 3-9 9H9v-3Z")]),
    ("pencil", &[(Stroke, "M4 20l1-4L16 5l3 3L8 19ZM14 7l3 3")]),
    ("paperplane", &[(Stroke, "M3.5 11L20.5 3.5 13 20.5 11 13ZM11 13l9.5-9.5")]),
    ("paperplane.fill", &[(Fill, "M3.5 11L20.5 3.5 13 20.5 11 13Z")]),
    ("lock", &[(Stroke, "M6 11h12v9H6ZM8.5 11V8a3.5 3.5 0 0 1 7 0v3")]),
    ("lock.fill", &[(Fill, "M6 11h12v9H6Z"), (Stroke, "M8.5 11V8a3.5 3.5 0 0 1 7 0v3")]),
    ("lock.open", &[(Stroke, "M6 11h12v9H6ZM8.5 11V8a3.5 3.5 0 0 1 7 0")]),
    ("person", &[(Stroke, PERSON)]),
    ("person.fill", &[(Fill, PERSON)]),
    ("house", &[(Stroke, "M4 11l8-7 8 7M6 9.5V20h12V9.5")]),
    ("bell", &[(Stroke, "M6 16v-5a6 6 0 0 1 12 0v5l1.5 2h-15ZM10 20.5a2 2 0 0 0 4 0")]),
    ("bolt", &[(Stroke, BOLT)]),
    ("bolt.fill", &[(Fill, BOLT)]),
    ("calendar", &[(Stroke, "M4 6h16v14H4ZM4 10h16M8 3.5v4M16 3.5v4")]),
    ("bubble.left", &[(Stroke, BUBBLE)]),
    ("bubble.left.fill", &[(Fill, BUBBLE)]),
    ("photo", &[(Stroke, "M3 5h18v14H3ZM3 16l5-5 4 4 3-3 6 6"), (Fill, "M17 9a1.5 1.5 0 1 1-3 0a1.5 1.5 0 1 1 3 0Z")]),
    ("terminal", &[(Stroke, "M3 4.5h18v15H3ZM7 9.5l3 2.5-3 2.5M12.5 15H17")]),
    (
        "list.bullet",
        &[
            (
                Fill,
                "M6.2 7a1.2 1.2 0 1 1-2.4 0a1.2 1.2 0 1 1 2.4 0ZM6.2 12a1.2 1.2 0 1 1-2.4 0a1.2 1.2 0 1 1 2.4 0ZM6.2 17a1.2 1.2 0 1 1-2.4 0a1.2 1.2 0 1 1 2.4 0Z",
            ),
            (Stroke, "M9 7h11M9 12h11M9 17h11"),
        ],
    ),
    ("line.3.horizontal", &[(Stroke, "M4 7h16M4 12h16M4 17h16")]),
    ("ellipsis", &[(Fill, DOTS)]),
    ("sidebar.left", &[(Stroke, SIDEBAR), (Stroke, "M9.5 4.5v15")]),
    ("sidebar.right", &[(Stroke, SIDEBAR), (Stroke, "M14.5 4.5v15")]),
    (
        "slider.horizontal.3",
        &[
            (Stroke, "M4 6h16M4 12h16M4 18h16"),
            (
                Fill,
                "M11 6a2 2 0 1 1-4 0a2 2 0 1 1 4 0ZM17 12a2 2 0 1 1-4 0a2 2 0 1 1 4 0ZM9 18a2 2 0 1 1-4 0a2 2 0 1 1 4 0Z",
            ),
        ],
    ),
    (
        "link",
        &[(
            Stroke,
            "M10 14a4 4 0 0 0 5.66 0l3-3a4 4 0 0 0-5.66-5.66l-1 1M14 10a4 4 0 0 0-5.66 0l-3 3a4 4 0 0 0 5.66 5.66l1-1",
        )],
    ),
    ("square.grid.2x2", &[(Stroke, "M4 4h7v7H4ZM13 4h7v7h-7ZM4 13h7v7H4ZM13 13h7v7h-7Z")]),
    ("eye", &[(Stroke, EYE), (Stroke, "M15 12a3 3 0 1 1-6 0a3 3 0 1 1 6 0Z")]),
    ("eye.slash", &[(Stroke, EYE), (Stroke, "M15 12a3 3 0 1 1-6 0a3 3 0 1 1 6 0ZM4 4l16 16")]),
    ("tray", &[(Stroke, "M3 13l3-8h12l3 8v6H3ZM3 13h5l1 2h6l1-2h5")]),
    ("bookmark", &[(Stroke, BOOKMARK)]),
    ("bookmark.fill", &[(Fill, BOOKMARK)]),
    ("tag", &[(Stroke, "M3.5 3.5h8l9 9-8 8-9-9Z"), (Fill, "M9.2 8a1.2 1.2 0 1 1-2.4 0a1.2 1.2 0 1 1 2.4 0Z")]),
    ("flag", &[(Stroke, "M5 21V4M5 4h12l-2 4 2 4H5")]),
    ("flag.fill", &[(Stroke, "M5 21V4"), (Fill, "M5 4h12l-2 4 2 4H5Z")]),
    ("cloud", &[(Stroke, "M7 18.5a4.5 4.5 0 0 1-.5-9A6 6 0 0 1 18 8.5a5 5 0 0 1-1 10Z")]),
    ("mic", &[(Stroke, "M12 3a3 3 0 0 1 3 3v6a3 3 0 0 1-6 0V6a3 3 0 0 1 3-3ZM6 11a6 6 0 0 0 12 0M12 17v4")]),
    ("speaker.wave.2", &[(Stroke, "M4 9.5h3.5L12 5.5v13l-4.5-4H4ZM15.5 9a4 4 0 0 1 0 6M18.5 6.5a7.5 7.5 0 0 1 0 11")]),
];

const STAR: &str =
    "M12 3.1L14.41 9.28L21.04 9.66L15.9 13.87L17.58 20.29L12 16.7L6.42 20.29L8.1 13.87L2.96 9.66L9.59 9.28Z";
const HEART: &str = "M12 20C8.5 17.5 3.5 14 3.5 9.25C3.5 6.5 5.5 4.5 8 4.5C9.75 4.5 11.1 5.4 12 6.8C12.9 5.4 14.25 4.5 16 4.5C18.5 4.5 20.5 6.5 20.5 9.25C20.5 14 15.5 17.5 12 20Z";
const SQUARE: &str = "M7 4h10a3 3 0 0 1 3 3v10a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3V7a3 3 0 0 1 3-3Z";
const PLAY: &str = "M7 4.5v15l12.5-7.5Z";
const FOLDER: &str =
    "M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v9.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5Z";
const PERSON: &str = "M15.5 7.5a3.5 3.5 0 1 1-7 0a3.5 3.5 0 1 1 7 0ZM5 20c0-3.9 3.1-6 7-6s7 2.1 7 6Z";
const BOLT: &str = "M13 2.5L5 13.5h6.5L10.5 21.5 19 10.5h-6.5Z";
const BUBBLE: &str = "M4 5h16v11H10l-4.5 3.5V16H4Z";
const DOTS: &str = "M7.1 12a1.6 1.6 0 1 1-3.2 0a1.6 1.6 0 1 1 3.2 0ZM13.6 12a1.6 1.6 0 1 1-3.2 0a1.6 1.6 0 1 1 3.2 0ZM20.1 12a1.6 1.6 0 1 1-3.2 0a1.6 1.6 0 1 1 3.2 0Z";
const DOTS_SMALL: &str = "M9.1 12a1.1 1.1 0 1 1-2.2 0a1.1 1.1 0 1 1 2.2 0ZM13.1 12a1.1 1.1 0 1 1-2.2 0a1.1 1.1 0 1 1 2.2 0ZM17.1 12a1.1 1.1 0 1 1-2.2 0a1.1 1.1 0 1 1 2.2 0Z";
const SIDEBAR: &str = "M5 4.5h14a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-11a2 2 0 0 1 2-2Z";
const BOOKMARK: &str = "M7 3.5h10v17l-5-4-5 4Z";
const EYE: &str = "M2.5 12C5 7.5 8.5 5.5 12 5.5s7 2 9.5 6.5c-2.5 4.5-6 6.5-9.5 6.5S5 16.5 2.5 12Z";

/// A drawing: its parts as paths, and the box they take up (units).
struct Drawing {
    parts: Vec<(Ink, BezPath)>,
    bounds: kurbo::Rect,
}

/// A gear of eight teeth and a hole, drawn in code.
fn gear() -> BezPath {
    let (outer, inner, teeth) = (9.5f64, 7.0f64, 8);
    let mut p = BezPath::new();
    let at = |r: f64, a: f64| Point::new(12.0 + r * a.cos(), 12.0 + r * a.sin());
    let step = std::f64::consts::TAU / f64::from(teeth);
    for k in 0..teeth {
        let a = f64::from(k) * step;
        // A tooth takes the middle of its step at the outer radius.
        let (a0, a1, a2, a3) = (a - step * 0.5, a - step * 0.22, a + step * 0.22, a + step * 0.5);
        if k == 0 {
            p.move_to(at(inner, a0));
        }
        p.line_to(at(inner, a1 - step * 0.06));
        p.line_to(at(outer, a1));
        p.line_to(at(outer, a2));
        p.line_to(at(inner, a2 + step * 0.06));
        p.line_to(at(inner, a3));
    }
    p.close_path();
    p.extend(kurbo::Circle::new((12.0, 12.0), 3.0).to_path(0.05));
    p
}

/// The drawing for `name`, parsed once.
fn drawing(name: &str) -> Option<&'static Drawing> {
    static DRAWINGS: OnceLock<Vec<(&'static str, Drawing)>> = OnceLock::new();
    let all = DRAWINGS.get_or_init(|| {
        SYMBOLS
            .iter()
            .map(|(name, parts)| {
                let parts: Vec<(Ink, BezPath)> = parts
                    .iter()
                    .map(|&(ink, d)| {
                        (ink, if d.is_empty() { gear() } else { BezPath::from_svg(d).unwrap_or_default() })
                    })
                    .collect();
                // As tall as the grid's middle twenty units, as symbols
                // line up with text; as wide as the ink, and a margin.
                let ink = parts.iter().map(|(_, p)| p.bounding_box()).reduce(|a, b| a.union(b)).unwrap_or_default();
                let bounds =
                    kurbo::Rect::new(ink.x0 - 1.5, (ink.y0 - 1.0).min(2.0), ink.x1 + 1.5, (ink.y1 + 1.0).max(22.0));
                (*name, Drawing { parts, bounds })
            })
            .collect()
    });
    all.iter().find(|(n, _)| *n == name).map(|(_, d)| d)
}

// NSImageSymbolConfiguration.

/// What a configuration sets: each field only if it was given.
#[derive(Clone, Default)]
pub(crate) struct Config {
    point_size: Option<f64>,
    weight: Option<f64>,
    scale: Option<NSImageSymbolScale>,
    text_style: Option<Retained<NSString>>,
    colors: Option<Retained<NSArray<NSColor>>>,
    /// Multicolor, monochrome or hierarchical, as asked.
    rendering: Option<u8>,
}

impl Config {
    /// `other`'s settings over these.
    fn applying(&self, other: &Config) -> Config {
        Config {
            point_size: other.point_size.or(self.point_size),
            weight: other.weight.or(self.weight),
            scale: other.scale.or(self.scale),
            text_style: other.text_style.clone().or_else(|| self.text_style.clone()),
            colors: other.colors.clone().or_else(|| self.colors.clone()),
            rendering: other.rendering.or(self.rendering),
        }
    }

    /// Points a full square of the drawing takes.
    fn size(&self) -> f64 {
        let base = match (self.point_size, &self.text_style) {
            (Some(size), _) => size,
            (None, Some(style)) => {
                let style: &NSFontTextStyle = style;
                // SAFETY: a text style name and no options.
                unsafe { NSFont::preferredFontForTextStyle_options(style, &NSDictionary::new()) }.pointSize()
            }
            (None, None) => 13.0,
        };
        let scale = match self.scale {
            Some(NSImageSymbolScale::Small) => 0.8,
            Some(NSImageSymbolScale::Large) => 1.3,
            _ => 1.0,
        };
        base * scale * 16.0 / 13.0
    }

    /// Stroke width in grid units: 2 at regular weight.
    fn stroke(&self) -> f64 {
        (2.0 * (1.0 + 0.9 * self.weight.unwrap_or(0.0))).clamp(0.6, 4.0)
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a configuration
    // doesn't change once made.
    #[unsafe(super(NSObject))]
    #[name = "NSImageSymbolConfiguration"]
    #[ivars = Config]
    pub(crate) struct NSImageSymbolConfigurationImpl;

    impl NSImageSymbolConfigurationImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(Config::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(configurationWithPointSize:weight:scale:))]
        fn with_size_weight_scale(size: f64, weight: f64, scale: NSImageSymbolScale) -> Retained<NSImageSymbolConfiguration> {
            config(Config { point_size: Some(size), weight: Some(weight), scale: Some(scale), ..Default::default() })
        }

        #[unsafe(method_id(configurationWithPointSize:weight:))]
        fn with_size_weight(size: f64, weight: f64) -> Retained<NSImageSymbolConfiguration> {
            config(Config { point_size: Some(size), weight: Some(weight), ..Default::default() })
        }

        #[unsafe(method_id(configurationWithTextStyle:scale:))]
        fn with_text_style_scale(style: &NSString, scale: NSImageSymbolScale) -> Retained<NSImageSymbolConfiguration> {
            config(Config { text_style: Some(style.copy()), scale: Some(scale), ..Default::default() })
        }

        #[unsafe(method_id(configurationWithTextStyle:))]
        fn with_text_style(style: &NSString) -> Retained<NSImageSymbolConfiguration> {
            config(Config { text_style: Some(style.copy()), ..Default::default() })
        }

        #[unsafe(method_id(configurationWithScale:))]
        fn with_scale(scale: NSImageSymbolScale) -> Retained<NSImageSymbolConfiguration> {
            config(Config { scale: Some(scale), ..Default::default() })
        }

        #[unsafe(method_id(configurationPreferringMonochrome))]
        fn preferring_monochrome() -> Retained<NSImageSymbolConfiguration> {
            config(Config { rendering: Some(1), ..Default::default() })
        }

        #[unsafe(method_id(configurationPreferringHierarchical))]
        fn preferring_hierarchical() -> Retained<NSImageSymbolConfiguration> {
            config(Config { rendering: Some(2), ..Default::default() })
        }

        #[unsafe(method_id(configurationPreferringMulticolor))]
        fn preferring_multicolor() -> Retained<NSImageSymbolConfiguration> {
            config(Config { rendering: Some(3), ..Default::default() })
        }

        #[unsafe(method_id(configurationWithHierarchicalColor:))]
        fn with_hierarchical_color(color: &NSColor) -> Retained<NSImageSymbolConfiguration> {
            let colors = NSArray::from_slice(&[color]);
            config(Config { colors: Some(colors), rendering: Some(2), ..Default::default() })
        }

        #[unsafe(method_id(configurationWithPaletteColors:))]
        fn with_palette_colors(colors: &NSArray<NSColor>) -> Retained<NSImageSymbolConfiguration> {
            config(Config { colors: Some(colors.copy()), ..Default::default() })
        }

        #[unsafe(method_id(configurationByApplyingConfiguration:))]
        fn by_applying(&self, other: &NSImageSymbolConfiguration) -> Retained<NSImageSymbolConfiguration> {
            config(self.ivars().applying(config_of(other)))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSImageSymbolConfiguration> {
            config(self.ivars().clone())
        }
    }

    unsafe impl NSObjectProtocol for NSImageSymbolConfigurationImpl {}

    unsafe impl NSCopying for NSImageSymbolConfigurationImpl {}
);

fn config(c: Config) -> Retained<NSImageSymbolConfiguration> {
    crate::load_shell::<NSImageSymbolConfiguration>();
    let this = NSImageSymbolConfigurationImpl::alloc().set_ivars(c);
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSImageSymbolConfigurationImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSImageSymbolConfigurationImpl is the class
    // NSImageSymbolConfiguration names.
    unsafe { Retained::cast_unchecked(this) }
}

pub(crate) fn config_of(c: &NSImageSymbolConfiguration) -> &Config {
    // SAFETY: every NSImageSymbolConfiguration is Sidestep's.
    unsafe { &*(c as *const NSImageSymbolConfiguration).cast::<NSImageSymbolConfigurationImpl>() }.ivars()
}

/// A configuration with nothing set, as a plain image reports.
pub(crate) fn empty_config() -> Retained<NSImageSymbolConfiguration> {
    config(Config::default())
}

/// What makes an image a symbol image: its name and configuration.
#[derive(Clone)]
pub(crate) struct Symbol {
    name: &'static str,
    config: Retained<NSImageSymbolConfiguration>,
    description: Option<Retained<NSString>>,
}

impl Symbol {
    pub fn config(&self) -> &Retained<NSImageSymbolConfiguration> {
        &self.config
    }
}

/// The symbol image `name` in `config`, if Sidestep has a drawing for it.
pub(crate) fn image(
    name: &str,
    description: Option<&NSString>,
    config: Retained<NSImageSymbolConfiguration>,
) -> Option<Retained<NSImage>> {
    let (&(name, _), drawing) = SYMBOLS.iter().find(|(n, _)| *n == name).zip(drawing(name))?;
    let c = config_of(&config);
    // Grid units to points.
    let k = c.size() / 24.0;
    let bounds = drawing.bounds;
    let size = NSSize::new((bounds.width() * k).round().max(1.0), (bounds.height() * k).round().max(1.0));
    let stroke = c.stroke();
    let handler = RcBlock::new(move |rect: NSRect| -> Bool {
        draw(drawing, stroke, rect);
        Bool::YES
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(size, true, &handler);
    image.setTemplate(true);
    image.setAccessibilityDescription(description);
    crate::image::set_symbol(
        &image,
        Symbol { name, config, description: description.map(|d| NSString::from_str(&d.to_string())) },
    );
    Some(image)
}

/// The same symbol in `config` applied over its own.
pub(crate) fn reconfigured(symbol: &Symbol, config: &NSImageSymbolConfiguration) -> Option<Retained<NSImage>> {
    let merged = self::config(config_of(&symbol.config).applying(config_of(config)));
    image(symbol.name, symbol.description.as_deref(), merged)
}

/// Draw `d` into `rect` (a flipped context), its box fitted to the
/// rectangle, in black.
fn draw(d: &Drawing, stroke: f64, rect: NSRect) {
    let b = d.bounds;
    let k = (rect.size.width / b.width()).min(rect.size.height / b.height());
    let place =
        kurbo::Affine::translate((rect.origin.x - b.x0 * k, rect.origin.y - b.y0 * k)) * kurbo::Affine::scale(k);
    let context = NSGraphicsContext::currentContext();
    NSColor::blackColor().set();
    for (ink, path) in &d.parts {
        let p = crate::path::from_bez(&(place * path.clone()));
        p.setLineWidth(stroke * k);
        p.setLineCapStyle(NSLineCapStyle::Round);
        p.setLineJoinStyle(NSLineJoinStyle::Round);
        let erase = matches!(ink, Erase | EraseFill);
        if let Some(c) = &context {
            c.setCompositingOperation(if erase {
                NSCompositingOperation::DestinationOut
            } else {
                NSCompositingOperation::SourceOver
            });
        }
        match ink {
            Stroke | Erase => p.stroke(),
            Fill | EraseFill => p.fill(),
        }
    }
    if let Some(c) = &context {
        c.setCompositingOperation(NSCompositingOperation::SourceOver);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_drawing_parses_and_keeps_to_its_grid() {
        for (name, parts) in SYMBOLS {
            for (_, d) in parts.iter() {
                if !d.is_empty() {
                    assert!(BezPath::from_svg(d).is_ok(), "{name}: {d}");
                }
            }
            let b = drawing(name).expect("a drawing").bounds;
            assert!(b.x0 >= -1.5 && b.y0 >= -1.5 && b.x1 <= 25.5 && b.y1 <= 25.5, "{name}: {b:?}");
            assert!(b.width() > 2.0 && b.height() > 2.0, "{name}: {b:?}");
        }
    }
}
