//! The string values of AppKit's attribute-name constants, as macOS prints
//! them. (The text engine's keys are checked with its tests.)
#![allow(deprecated)]

use objc2_app_kit::{
    NSAdaptiveImageGlyphAttributeName, NSBackgroundColorAttributeName, NSCharacterShapeAttributeName,
    NSFontAttributeName, NSForegroundColorAttributeName, NSTextHighlightColorSchemeAttributeName,
    NSTextHighlightStyleAttributeName, NSWritingToolsExclusionAttributeName,
};

use sidestep as _;

#[test]
fn attribute_name_values() {
    let names = unsafe {
        [
            (NSFontAttributeName, "NSFont"),
            (NSForegroundColorAttributeName, "NSColor"),
            (NSBackgroundColorAttributeName, "NSBackgroundColor"),
            (NSCharacterShapeAttributeName, "NSCharacterShape"),
            (NSTextHighlightStyleAttributeName, "NSTextHighlightStyle"),
            (NSTextHighlightColorSchemeAttributeName, "NSTextHighlightColorScheme"),
            (NSAdaptiveImageGlyphAttributeName, "CTAdaptiveImageProvider"),
            (NSWritingToolsExclusionAttributeName, "WTWritingToolsPreserved"),
        ]
    };
    for (name, value) in names {
        assert_eq!(name.to_string(), value);
    }
}
