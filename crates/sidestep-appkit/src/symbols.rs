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
    (
        "exclamationmark.octagon",
        &[
            (Stroke, "M8.3 3h7.4L21 8.3v7.4L15.7 21H8.3L3 15.7V8.3Z"),
            (Stroke, "M12 7.5v5.5"),
            (Fill, "M13 16.5a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z"),
        ],
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
    ("doc.text", &[(Stroke, "M6 3h8l4 4v14H6ZM14 3v4h4M9 11.5h6M9 14.5h6M9 17.5h4")]),
    (
        "quote.opening",
        &[(
            Fill,
            "M10.5 6.5C7 7.5 4.5 10 4.5 14v3.5H10V12H7.3c.3-1.9 1.5-3.2 3.2-3.8ZM19.5 6.5c-3.5 1-6 3.5-6 7.5v3.5H19V12h-2.7c.3-1.9 1.5-3.2 3.2-3.8Z",
        )],
    ),
    (
        "lightbulb",
        &[(
            Stroke,
            "M9.5 18.5h5M10 21h4M12 3a6 6 0 0 0-3.6 10.8c.7.6 1.1 1.4 1.1 2.2v.5h5V16c0-.8.4-1.6 1.1-2.2A6 6 0 0 0 12 3Z",
        )],
    ),
    (
        "hand.raised",
        &[(
            Stroke,
            "M8 12.5V6.5a1.5 1.5 0 0 1 3 0v5M11 11.5V4.5a1.5 1.5 0 0 1 3 0v7M14 11.5V6a1.5 1.5 0 0 1 3 0v8.5a6.5 6.5 0 0 1-6.5 6.5h-.6a6 6 0 0 1-4.7-2.3l-2.6-3.4a1.6 1.6 0 0 1 2.5-2L8 15.5V12.5",
        )],
    ),
    (
        "ladybug",
        &[
            (
                Stroke,
                "M18 14.5a6 6.5 0 1 1-12 0a6 6.5 0 1 1 12 0ZM9.5 8.3a2.5 2.5 0 0 1 5 0M12 8v13M6.3 12L3.5 10.5M6 15.5H3M6.8 19l-2.3 1.5M17.7 12l2.8-1.5M18 15.5h3M17.2 19l2.3 1.5",
            ),
            (
                Fill,
                "M10 12.5a1 1 0 1 1-2 0a1 1 0 1 1 2 0ZM16 12.5a1 1 0 1 1-2 0a1 1 0 1 1 2 0ZM10 17a1 1 0 1 1-2 0a1 1 0 1 1 2 0ZM16 17a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z",
            ),
        ],
    ),
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
    (
        "list.bullet.rectangle",
        &[
            (
                Stroke,
                "M5 4h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2ZM10.5 9h6.5M10.5 12h6.5M10.5 15h6.5",
            ),
            (
                Fill,
                "M8.6 9a1 1 0 1 1-2 0a1 1 0 1 1 2 0ZM8.6 12a1 1 0 1 1-2 0a1 1 0 1 1 2 0ZM8.6 15a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z",
            ),
        ],
    ),
    (
        "checklist",
        &[(
            Stroke,
            "M3.5 6.5l1.5 1.5 3-3.5M3.5 13l1.5 1.5 3-3.5M3.5 19.5l1.5 1.5 3-3.5M11 7h9.5M11 13.5h9.5M11 20h9.5",
        )],
    ),
    ("text.alignleft", &[(Stroke, "M4 6h16M4 10h11M4 14h16M4 18h11")]),
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
    // Arrows.
    ("arrow.left.and.right", &[(Stroke, "M3 12h18M7 8l-4 4 4 4M17 8l4 4-4 4")]),
    ("arrow.up.right", &[(Stroke, "M7 17L17 7M9 7h8v8")]),
    ("arrow.triangle.branch", &[(Stroke, "M12 21v-7L7 7.5V4M12 14l5-6.5V4M4.5 6.5L7 4l2.5 2.5M14.5 6.5L17 4l2.5 2.5")]),
    (
        "arrow.triangle.2.circlepath",
        &[(Stroke, "M4.5 10.5A7.5 7.5 0 0 1 18 7.5M19.5 13.5A7.5 7.5 0 0 1 6 16.5M18 3.5v4h-4M6 20.5v-4h4")],
    ),
    (
        "arrow.trianglehead.2.clockwise.rotate.90",
        &[(Stroke, "M12 4a8 8 0 0 1 8 8M12 20a8 8 0 0 1-8-8M17.5 9.5L20 12l2.5-2.5M1.5 14.5L4 12l2.5 2.5")],
    ),
    // Status and progress.
    ("circle.dotted", &[(Fill, DOTTED_RING)]),
    ("stop.circle", &[(Stroke, CIRCLE), (Fill, "M9 9h6v6H9Z")]),
    (
        "exclamationmark.triangle.fill",
        &[
            (Fill, TRIANGLE),
            (Stroke, TRIANGLE),
            (Erase, "M12 9.5v4.5"),
            (EraseFill, "M13 17a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z"),
        ],
    ),
    (
        "exclamationmark.octagon",
        &[
            (Stroke, "M8.3 3h7.4L21 8.3v7.4L15.7 21H8.3L3 15.7V8.3Z"),
            (Stroke, "M12 7.5v6"),
            (Fill, "M13 16.5a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z"),
        ],
    ),
    (
        "gauge.with.needle",
        &[(Stroke, CIRCLE), (Stroke, "M12 13l3.5-4.5"), (Fill, "M13.5 13a1.5 1.5 0 1 1-3 0a1.5 1.5 0 1 1 3 0Z")],
    ),
    ("chart.bar", &[(Stroke, "M4 13h4v7H4ZM10 8h4v12h-4ZM16 4h4v16h-4Z")]),
    ("sparkle", &[(Fill, SPARKLE)]),
    ("sparkles", &[(Fill, SPARKLES)]),
    ("crown.fill", &[(Fill, CROWN), (Stroke, CROWN), (Stroke, "M5.5 20.5h13")]),
    // Things.
    ("command", &[(Stroke, COMMAND)]),
    (
        "cpu",
        &[(Stroke, "M7 7h10v10H7ZM10 10h4v4h-4ZM9.5 3.5V7M14.5 3.5V7M9.5 17v3.5M14.5 17v3.5M3.5 9.5H7M3.5 14.5H7M17 9.5h3.5M17 14.5h3.5")],
    ),
    ("paperclip", &[(Stroke, "M15.5 7.5l-7 7a2 2 0 0 0 2.8 2.8l7.7-7.7a4 4 0 0 0-5.7-5.7l-7.7 7.7a6 6 0 0 0 8.5 8.5l6.4-6.4")]),
    ("hammer", &[(Stroke, "M10 6.9L12.9 4 20 11.1 17.1 14ZM13.6 10.4L4.5 19.5")]),
    ("paintbrush", &[(Stroke, "M20 4c-1 3-4.5 7-7.5 9L11 11.5c2-3 6-6.5 9-7.5ZM10 13c-2 0-3.5 1.5-3.5 3.5 0 1.5-1 2.5-2.5 3 1 .5 2 .5 3 .5 3 0 5-2 5-4.5")]),
    ("archivebox", &[(Stroke, "M3.5 4.5h17v4h-17ZM5 8.5v11h14v-11M10 12h4")]),
    ("externaldrive", &[(Stroke, "M3 13l2.5-7.5h13L21 13v5H3ZM3 13h18"), (Fill, "M18 15.5a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z")]),
    (
        "cylinder.split.1x2",
        &[(Stroke, "M5 6c0-1.7 3.1-3 7-3s7 1.3 7 3-3.1 3-7 3-7-1.3-7-3ZM5 6v12c0 1.7 3.1 3 7 3s7-1.3 7-3V6M5 12c0 1.7 3.1 3 7 3s7-1.3 7-3")],
    ),
    (
        "folder.badge.plus",
        &[(Stroke, FOLDER), (EraseFill, "M23 16a5 5 0 1 1-10 0a5 5 0 1 1 10 0Z"), (Stroke, "M18 13.5v5M15.5 16h5")],
    ),
    ("doc.text", &[(Stroke, "M6 3h8l4 4v14H6ZM14 3v4h4M9 9h3M9 12.5h6M9 16h6")]),
    ("lightbulb", &[(Stroke, "M9 18h6M10 21h4M12 3a6 6 0 0 0-3.5 10.9c.6.5 1 1.2 1 2V16h5v-.1c0-.8.4-1.5 1-2A6 6 0 0 0 12 3Z")]),
    (
        "ladybug",
        &[
            (Stroke, "M18.5 14a6.5 6.5 0 1 1-13 0a6.5 6.5 0 1 1 13 0ZM12 7.5v13M9 7.5a3 3 0 0 1 6 0"),
            (Fill, "M10 12a1 1 0 1 1-2 0a1 1 0 1 1 2 0ZM16 12a1 1 0 1 1-2 0a1 1 0 1 1 2 0ZM10 16.5a1 1 0 1 1-2 0a1 1 0 1 1 2 0ZM16 16.5a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z"),
        ],
    ),
    (
        "hand.raised",
        &[(
            Stroke,
            "M8 13V6a1.5 1.5 0 0 1 3 0v5M11 11V4.5a1.5 1.5 0 0 1 3 0V11M14 11V5.5a1.5 1.5 0 0 1 3 0V12M17 12V8.5a1.5 1.5 0 0 1 3 0v6c0 4-2.5 6.5-6.5 6.5-3 0-4.5-1.5-6-4l-2.5-4a1.5 1.5 0 0 1 2.5-1.5L8 13",
        )],
    ),
    // People.
    ("person.crop.circle", &[(Stroke, CIRCLE), (Stroke, "M15 10a3 3 0 1 1-6 0a3 3 0 1 1 6 0ZM6.2 18.2c1.3-2 3.4-3.2 5.8-3.2s4.5 1.2 5.8 3.2")]),
    (
        "person.3",
        &[(
            Stroke,
            "M14.5 8a2.5 2.5 0 1 1-5 0a2.5 2.5 0 1 1 5 0ZM7 18.5c0-2.8 2.2-4.5 5-4.5s5 1.7 5 4.5M7 9.5a2 2 0 1 1-4 0a2 2 0 1 1 4 0ZM2 17c0-2 1.3-3.3 3-3.5M21 9.5a2 2 0 1 1-4 0a2 2 0 1 1 4 0ZM22 17c0-2-1.3-3.3-3-3.5",
        )],
    ),
    // Text and code.
    ("text.bubble", &[(Stroke, BUBBLE), (Stroke, "M8 9h8M8 12.5h5")]),
    ("text.alignleft", &[(Stroke, "M4 6h16M4 10h10M4 14h16M4 18h10")]),
    ("textformat.size", &[(Stroke, "M3 19L8.5 5l5.5 14M5 14h7M14.5 19l3.5-8.5 3.5 8.5M15.6 16.5h4.8")]),
    ("line.3.horizontal.decrease", &[(Stroke, "M4 7h16M7 12h10M10 17h4")]),
    ("chevron.left.forwardslash.chevron.right", &[(Stroke, "M8 7l-5 5 5 5M16 7l5 5-5 5M14 5l-4 14")]),
    ("chevron.left.slash.chevron.right", &[(Stroke, "M8 7l-5 5 5 5M16 7l5 5-5 5M13.5 6l-3 12")]),
    (
        "curlybraces",
        &[(
            Stroke,
            "M9 4c-2 0-2.5 1-2.5 3v2c0 1.5-.5 3-2.5 3 2 0 2.5 1.5 2.5 3v2c0 2 .5 3 2.5 3M15 4c2 0 2.5 1 2.5 3v2c0 1.5.5 3 2.5 3-2 0-2.5 1.5-2.5 3v2c0 2-.5 3-2.5 3",
        )],
    ),
    ("plus.forwardslash.minus", &[(Stroke, "M4.5 7.5h6M7.5 4.5v6M13.5 16.5h6M17 4L7 20")]),
    (
        "quote.opening",
        &[(
            Fill,
            "M5 11.5c0-3 1.5-5 4-6.5l.8 1.2C8.3 7.3 7.5 8.6 7.3 10H10v5.5H5ZM13 11.5c0-3 1.5-5 4-6.5l.8 1.2c-1.5 1.1-2.3 2.4-2.5 3.8H18v5.5h-5Z",
        )],
    ),
    (
        "checklist",
        &[(Stroke, "M4 6.5l1.5 1.5L8.5 5M4 13l1.5 1.5 3-3M11 6.5h9M11 13h9M11 19h9"), (Fill, "M7.5 19a1.2 1.2 0 1 1-2.4 0a1.2 1.2 0 1 1 2.4 0Z")],
    ),
    (
        "list.bullet.rectangle",
        &[
            (Stroke, "M3.5 4.5h17v15h-17ZM10 9h7M10 12h7M10 15h7"),
            (Fill, "M8 9a1 1 0 1 1-2 0a1 1 0 1 1 2 0ZM8 12a1 1 0 1 1-2 0a1 1 0 1 1 2 0ZM8 15a1 1 0 1 1-2 0a1 1 0 1 1 2 0Z"),
        ],
    ),
    // Layout.
    ("square.split.2x2", &[(Stroke, SQUARE), (Stroke, "M12 4v16M4 12h16")]),
    ("square.stack.3d.up", &[(Stroke, "M12 3l8.5 4.5L12 12 3.5 7.5ZM3.5 12L12 16.5 20.5 12M3.5 16.5L12 21l8.5-4.5")]),
];

const TRIANGLE: &str = "M12 3.5L21.5 20h-19Z";
const CROWN: &str = "M3 7.5L8 11.5 12 4.5 16 11.5 21 7.5 19 17.5H5Z";
/// ⌘: a square with a loop at each corner.
const COMMAND: &str = "M9 9h6v6H9ZM9 9V6.5A2.5 2.5 0 1 0 6.5 9H9M15 9V6.5A2.5 2.5 0 1 1 17.5 9H15M9 15v2.5A2.5 2.5 0 1 1 6.5 15H9M15 15v2.5a2.5 2.5 0 1 0 2.5-2.5H15";
const SPARKLE: &str = "M12 2.5C13.14 9.95 14.05 10.86 21.5 12C14.05 13.14 13.14 14.05 12 21.5C10.86 14.05 9.95 13.14 2.5 12C9.95 10.86 10.86 9.95 12 2.5Z";
const SPARKLES: &str = "M10 6C10.9 11.88 11.62 12.6 17.5 13.5C11.62 14.4 10.9 15.12 10 21C9.1 15.12 8.38 14.4 2.5 13.5C8.38 12.6 9.1 11.88 10 6ZM18.5 2.3C18.88 4.81 19.19 5.12 21.7 5.5C19.19 5.88 18.88 6.19 18.5 8.7C18.12 6.19 17.81 5.88 15.3 5.5C17.81 5.12 18.12 4.81 18.5 2.3ZM19 15.6C19.29 17.48 19.52 17.71 21.4 18C19.52 18.29 19.29 18.52 19 20.4C18.71 18.52 18.48 18.29 16.6 18C18.48 17.71 18.71 17.48 19 15.6Z";
/// Twelve dots around the circle.
const DOTTED_RING: &str = "M22.15 12a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM20.94 16.5a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM17.65 19.79a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM13.15 21a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM8.65 19.79a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM5.36 16.5a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM4.15 12a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM5.36 7.5a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM8.65 4.21a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM13.15 3a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM17.65 4.21a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0ZM20.94 7.5a1.15 1.15 0 1 1-2.3 0a1.15 1.15 0 1 1 2.3 0Z";

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
    let inks = Inks::of(c);
    let template = inks.is_none();
    let handler = RcBlock::new(move |rect: NSRect| -> Bool {
        draw(drawing, stroke, inks.as_ref(), rect);
        Bool::YES
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(size, true, &handler);
    // A symbol with colours of its own draws in them, as AppKit's does;
    // only a plain one is a template for views to tint.
    image.setTemplate(template);
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

/// The colours a configuration draws a symbol in, if it gives any.
///
/// A hierarchical colour inks a symbol's marks (what a plain drawing
/// knocks out of a filled shape, like the check of
/// `checkmark.circle.fill`) in the colour and the shape behind them in a
/// fainter shade of it; a palette gives the marks its first colour and the
/// shapes behind them its second. A drawing without such marks is all in
/// the first colour.
struct Inks {
    primary: Retained<NSColor>,
    /// The palette's second colour; else hierarchical shapes take the
    /// primary, fainter.
    secondary: Option<Retained<NSColor>>,
    hierarchical: bool,
}

/// How strong a hierarchical symbol's shapes behind its marks are.
const SECONDARY_ALPHA: f64 = 0.5;

impl Inks {
    fn of(c: &Config) -> Option<Inks> {
        let colors = c.colors.as_ref()?;
        let primary = colors.firstObject()?;
        let secondary = (colors.count() > 1).then(|| colors.objectAtIndex(1));
        Some(Inks { primary, secondary, hierarchical: c.rendering == Some(2) })
    }

    /// The colour for the shapes behind a layered drawing's marks, if they
    /// take one apart from the marks'.
    fn behind(&self) -> Option<Retained<NSColor>> {
        match &self.secondary {
            Some(second) => Some(second.clone()),
            None if self.hierarchical => {
                Some(self.primary.colorWithAlphaComponent(self.primary.alphaComponent() * SECONDARY_ALPHA))
            }
            None => None,
        }
    }
}

/// Draw `d` into `rect` (a flipped context), its box fitted to the
/// rectangle, in black, or in `inks`.
fn draw(d: &Drawing, stroke: f64, inks: Option<&Inks>, rect: NSRect) {
    let b = d.bounds;
    let k = (rect.size.width / b.width()).min(rect.size.height / b.height());
    let place =
        kurbo::Affine::translate((rect.origin.x - b.x0 * k, rect.origin.y - b.y0 * k)) * kurbo::Affine::scale(k);
    let context = NSGraphicsContext::currentContext();
    // With two colours a layered drawing's marks are inked over the shape
    // rather than knocked out of it.
    let layered = d.parts.iter().any(|(ink, _)| matches!(ink, Erase | EraseFill));
    let behind = inks.filter(|_| layered).and_then(Inks::behind);
    let primary = inks.map_or_else(NSColor::blackColor, |i| i.primary.clone());
    primary.set();
    for (ink, path) in &d.parts {
        let p = crate::path::from_bez(&(place * path.clone()));
        p.setLineWidth(stroke * k);
        p.setLineCapStyle(NSLineCapStyle::Round);
        p.setLineJoinStyle(NSLineJoinStyle::Round);
        let ink = match (&behind, *ink) {
            (Some(behind), Stroke | Fill) => {
                behind.set();
                *ink
            }
            (Some(_), mark) => {
                primary.set();
                if mark == Erase { Stroke } else { Fill }
            }
            (None, ink) => ink,
        };
        let ink = &ink;
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
